//! Tree ordering for repos and workspaces.
//!
//! Two independent orderings live side by side. The **manual** order is the
//! order of `AppState::repos` / `AppState::workspaces` itself — moving an item
//! rewrites those vectors, so it survives restarts for free. On top of it sits
//! an optional **built-in sort** ([`SortMode`]), applied at render time only:
//! turning a sort off falls straight back to the saved manual order.
//!
//! Every sort is *stable*, so the manual order is the tie-break — equal names
//! (or equal timestamps) keep the order the user arranged by hand.

use std::cmp::Reverse;

use serde::{Deserialize, Serialize};

use crate::repo::RepoEntry;
use crate::workspace::Workspace;

/// How a level of the tree is ordered. [`SortMode::Manual`] means "use the
/// saved order as-is"; the rest are computed views over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SortMode {
    /// The saved hand-arranged order (the vector's own order).
    #[default]
    Manual,
    NameAsc,
    NameDesc,
    AddedAsc,
    AddedDesc,
}

impl SortMode {
    /// The name-sort toggle: off → ascending → descending → off. Coming from a
    /// date sort it enters at ascending (the toggles are mutually exclusive —
    /// only one built-in sort is ever live).
    pub fn cycle_name(self) -> Self {
        match self {
            SortMode::NameAsc => SortMode::NameDesc,
            SortMode::NameDesc => SortMode::Manual,
            _ => SortMode::NameAsc,
        }
    }

    /// The added-date toggle, mirroring [`Self::cycle_name`].
    pub fn cycle_added(self) -> Self {
        match self {
            SortMode::AddedAsc => SortMode::AddedDesc,
            SortMode::AddedDesc => SortMode::Manual,
            _ => SortMode::AddedAsc,
        }
    }

    /// Whether a built-in sort is live (so the on-screen order is computed,
    /// not the saved one).
    pub fn is_sorted(self) -> bool {
        self != SortMode::Manual
    }

    /// Compact status indicator for the tree title; empty when manual, so a
    /// hand-ordered tree carries no chrome.
    pub fn indicator(self) -> &'static str {
        match self {
            SortMode::Manual => "",
            SortMode::NameAsc => "↑name",
            SortMode::NameDesc => "↓name",
            SortMode::AddedAsc => "↑added",
            SortMode::AddedDesc => "↓added",
        }
    }

    /// Human label for the palette and help text.
    pub fn label(self) -> &'static str {
        match self {
            SortMode::Manual => "manual order",
            SortMode::NameAsc => "name (A→Z)",
            SortMode::NameDesc => "name (Z→A)",
            SortMode::AddedAsc => "date added (oldest first)",
            SortMode::AddedDesc => "date added (newest first)",
        }
    }
}

/// Case-insensitive name key — "Zulu" must not sort before "alpha".
fn name_key(name: &str) -> String {
    name.to_lowercase()
}

/// Order `repos` in place for `mode` (a no-op for [`SortMode::Manual`]).
///
/// A repo added before `added_at` existed has `None`, which sorts as the
/// oldest — correct, since it predates the field.
pub fn sort_repos(repos: &mut [RepoEntry], mode: SortMode) {
    match mode {
        SortMode::Manual => {}
        SortMode::NameAsc => repos.sort_by_key(|r| name_key(&r.name)),
        SortMode::NameDesc => repos.sort_by_key(|r| Reverse(name_key(&r.name))),
        SortMode::AddedAsc => repos.sort_by_key(|r| r.added_at),
        SortMode::AddedDesc => repos.sort_by_key(|r| Reverse(r.added_at)),
    }
}

/// Order `workspaces` in place for `mode`, by name or by `created_at`.
pub fn sort_workspaces(workspaces: &mut [Workspace], mode: SortMode) {
    match mode {
        SortMode::Manual => {}
        SortMode::NameAsc => workspaces.sort_by_key(|w| name_key(&w.name)),
        SortMode::NameDesc => workspaces.sort_by_key(|w| Reverse(name_key(&w.name))),
        SortMode::AddedAsc => workspaces.sort_by_key(|w| w.created_at),
        SortMode::AddedDesc => workspaces.sort_by_key(|w| Reverse(w.created_at)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str, added: Option<u64>) -> RepoEntry {
        RepoEntry {
            id: name.to_string(),
            name: name.to_string(),
            path: format!("/tmp/{name}"),
            added_at: added,
        }
    }

    fn names(repos: &[RepoEntry]) -> Vec<&str> {
        repos.iter().map(|r| r.name.as_str()).collect()
    }

    #[test]
    fn manual_is_a_no_op() {
        let mut repos = vec![repo("zulu", Some(1)), repo("alpha", Some(2))];
        sort_repos(&mut repos, SortMode::Manual);
        assert_eq!(names(&repos), ["zulu", "alpha"]);
    }

    #[test]
    fn name_sort_is_case_insensitive_both_ways() {
        let mut repos = vec![repo("Zulu", None), repo("alpha", None), repo("Mike", None)];
        sort_repos(&mut repos, SortMode::NameAsc);
        assert_eq!(names(&repos), ["alpha", "Mike", "Zulu"]);
        sort_repos(&mut repos, SortMode::NameDesc);
        assert_eq!(names(&repos), ["Zulu", "Mike", "alpha"]);
    }

    #[test]
    fn added_sort_puts_undated_repos_oldest() {
        let mut repos = vec![repo("new", Some(200)), repo("legacy", None), repo("old", Some(100))];
        sort_repos(&mut repos, SortMode::AddedAsc);
        assert_eq!(names(&repos), ["legacy", "old", "new"]);
        sort_repos(&mut repos, SortMode::AddedDesc);
        assert_eq!(names(&repos), ["new", "old", "legacy"]);
    }

    #[test]
    fn sorts_are_stable_so_manual_order_breaks_ties() {
        // Two repos share a name and a timestamp: whichever the user placed
        // first must stay first, in both directions.
        let mut repos = vec![repo("dup", Some(5)), repo("dup", Some(5))];
        repos[0].id = "first".into();
        repos[1].id = "second".into();
        for mode in [SortMode::NameAsc, SortMode::NameDesc, SortMode::AddedAsc, SortMode::AddedDesc] {
            let mut copy = repos.clone();
            sort_repos(&mut copy, mode);
            assert_eq!(copy[0].id, "first", "{mode:?} reordered equal keys");
        }
    }

    #[test]
    fn name_toggle_cycles_off_through_desc() {
        let m = SortMode::default();
        assert_eq!(m, SortMode::Manual);
        let m = m.cycle_name();
        assert_eq!(m, SortMode::NameAsc);
        let m = m.cycle_name();
        assert_eq!(m, SortMode::NameDesc);
        assert_eq!(m.cycle_name(), SortMode::Manual);
    }

    #[test]
    fn the_two_toggles_are_mutually_exclusive() {
        // Pressing the date toggle while a name sort is live switches sorts
        // rather than layering them.
        assert_eq!(SortMode::NameDesc.cycle_added(), SortMode::AddedAsc);
        assert_eq!(SortMode::AddedDesc.cycle_name(), SortMode::NameAsc);
    }

    #[test]
    fn only_manual_has_no_indicator() {
        assert_eq!(SortMode::Manual.indicator(), "");
        for mode in [SortMode::NameAsc, SortMode::NameDesc, SortMode::AddedAsc, SortMode::AddedDesc] {
            assert!(!mode.indicator().is_empty(), "{mode:?} needs an indicator");
            assert!(mode.is_sorted());
        }
        assert!(!SortMode::Manual.is_sorted());
    }
}
