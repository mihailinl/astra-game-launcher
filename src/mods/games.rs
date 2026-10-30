// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `games.tsv`: where the mod managers keep a Steam game's profiles, generated from Thunderstore's
//! ecosystem schema by `tools/gen-mods-games.py`. Data, not code: a game Thunderstore adds needs
//! only the script run again.

/// One row: `appid  r2_folder  community  loader`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GameRow {
    pub appid: u32,
    /// r2modman's and Thunderstore Mod Manager's per-game folder (`internalFolderName`).
    pub r2_folder: &'static str,
    /// The Thunderstore community: Gale's per-game folder.
    pub community: &'static str,
    /// The schema's `packageLoader`: `bepinex`, `melonloader`, `recursive-melonloader`, …
    pub loader: &'static str,
}

const TABLE: &str = include_str!("games.tsv");

/// Every row of the table, in its order (by appid).
pub(crate) fn rows() -> impl Iterator<Item = GameRow> {
    TABLE.lines().filter_map(|line| {
        if line.starts_with('#') || line.trim().is_empty() {
            return None;
        }
        let mut cells = line.split('\t');
        let appid = cells.next()?.parse().ok()?;
        let (r2_folder, community, loader) = (cells.next()?, cells.next()?, cells.next()?);
        Some(GameRow {
            appid,
            r2_folder,
            community,
            loader,
        })
    })
}

/// The row of Steam app `appid`, if Thunderstore knows the game.
pub(crate) fn row(appid: u32) -> Option<GameRow> {
    rows().find(|r| r.appid == appid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_whole_and_sorted() {
        let all: Vec<GameRow> = rows().collect();
        assert!(all.len() > 300, "{}", all.len());
        assert!(all.windows(2).all(|w| w[0].appid < w[1].appid));
        let data_lines = TABLE
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .count();
        assert_eq!(all.len(), data_lines, "every line parses");
        assert!(
            all.iter()
                .all(|r| !r.r2_folder.is_empty() && !r.community.is_empty())
        );
    }
}
