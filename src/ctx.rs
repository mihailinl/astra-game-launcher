// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Cancellation and progress for long operations.

use serde::Serialize;

use crate::error::{LauncherError, Result, codes};

/// One progress step. `stage` is a code: "profile-files" or "game-files".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Progress {
    pub stage: &'static str,
    pub done: u64,
    pub total: u64,
}

/// What a long operation asks of its caller: may it go on, and here is how far it got.
pub struct Ctx<'a> {
    pub cancel: &'a dyn Fn() -> bool,
    pub progress: &'a dyn Fn(Progress),
}

fn never() -> bool {
    false
}

fn ignore(_: Progress) {}

impl Ctx<'static> {
    /// Never cancelled, reports nothing.
    pub fn silent() -> Ctx<'static> {
        Ctx { cancel: &never, progress: &ignore }
    }
}

impl Ctx<'_> {
    pub(crate) fn check(&self) -> Result<()> {
        if (self.cancel)() {
            Err(LauncherError::new(codes::CANCELLED))
        } else {
            Ok(())
        }
    }

    pub(crate) fn report(&self, stage: &'static str, done: u64, total: u64) {
        (self.progress)(Progress { stage, done, total });
    }
}
