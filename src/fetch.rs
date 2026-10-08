// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! HTTP is injected: the launcher asks a [`Fetch`] for bytes and checks them itself.

use crate::digest::{normalize_hex, sha256_hex};
use crate::error::{LauncherError, Result, codes};

/// The largest download the launcher asks for: the zip cap.
pub const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;

pub trait Fetch {
    /// GETs `url`. Must refuse with `too-large` rather than return more than `max_bytes`.
    fn get(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>>;
}

/// Downloads `url` and checks it against `expected_sha256` before returning a byte of it.
pub fn fetch_verified(fetch: &dyn Fetch, url: &str, expected_sha256: &str, max_bytes: u64) -> Result<Vec<u8>> {
    let Some(want) = normalize_hex(expected_sha256) else {
        return Err(LauncherError::new(codes::DIGEST_MISMATCH)
            .with("want", expected_sha256)
            .with("reason", "not-a-sha256"));
    };
    let bytes = fetch.get(url, max_bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(LauncherError::new(codes::TOO_LARGE).with("limit", max_bytes.to_string()));
    }
    let got = sha256_hex(&bytes);
    if got != want {
        return Err(LauncherError::new(codes::DIGEST_MISMATCH).with("want", want).with("got", got));
    }
    Ok(bytes)
}

/// The command-line program's HTTP client: HTTPS only, rustls, bounded body.
#[cfg(feature = "cli")]
pub struct UreqFetch {
    agent: ureq::Agent,
}

#[cfg(feature = "cli")]
impl Default for UreqFetch {
    fn default() -> Self {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .timeout_global(Some(std::time::Duration::from_secs(600)))
            .user_agent(concat!("astra-game-launcher/", env!("CARGO_PKG_VERSION")))
            .build();
        UreqFetch { agent: config.into() }
    }
}

#[cfg(feature = "cli")]
impl Fetch for UreqFetch {
    fn get(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>> {
        if !url.starts_with("https://") {
            return Err(LauncherError::new(codes::FETCH_FAILED).with("reason", "https-only"));
        }
        let failed = |e: ureq::Error| match e {
            ureq::Error::BodyExceedsLimit(_) => {
                LauncherError::new(codes::TOO_LARGE).with("limit", max_bytes.to_string())
            }
            ureq::Error::StatusCode(s) => LauncherError::new(codes::FETCH_FAILED).with("status", s.to_string()),
            other => LauncherError::new(codes::FETCH_FAILED).with("detail", other.to_string()),
        };
        let mut resp = self.agent.get(url).call().map_err(failed)?;
        resp.body_mut().with_config().limit(max_bytes).read_to_vec().map_err(failed)
    }
}
