// SPDX-License-Identifier: GPL-3.0-or-later
//! Fuzzy search over the catalog using `nucleo-matcher`.
//!
//! Score = fuzzy score on the name (keywords count at half weight)
//!       + prefix bonus + frecency boost from the launch history.
//! An empty query lists the most-used entries first, then alphabetically.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::catalog::{AppEntry, Catalog, LaunchHistory};

pub struct Searcher {
    matcher: Matcher,
    buf: Vec<char>,
}

impl Default for Searcher {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit<'a> {
    pub entry: &'a AppEntry,
    pub score: u32,
}

const PREFIX_BONUS: u32 = 60;
const WORD_PREFIX_BONUS: u32 = 25;

impl Searcher {
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
            buf: Vec::new(),
        }
    }

    pub fn search<'a>(
        &mut self,
        catalog: &'a Catalog,
        history: &LaunchHistory,
        query: &str,
        limit: usize,
        now: u64,
    ) -> Vec<Hit<'a>> {
        let query = query.trim();
        if query.is_empty() {
            let mut hits: Vec<Hit<'a>> = catalog
                .entries()
                .iter()
                .map(|e| Hit {
                    entry: e,
                    score: history.boost(&e.target, now),
                })
                .collect();
            // Stable sort keeps alphabetical order among equal scores.
            hits.sort_by_key(|h| std::cmp::Reverse(h.score));
            hits.truncate(limit);
            return hits;
        }

        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let q_lower = query.to_lowercase();
        let mut hits = Vec::new();
        for e in catalog.entries() {
            let name_score =
                pattern.score(Utf32Str::new(&e.name, &mut self.buf), &mut self.matcher);
            let kw_score = e
                .keywords
                .iter()
                .filter_map(|k| pattern.score(Utf32Str::new(k, &mut self.buf), &mut self.matcher))
                .max()
                .map(|s| s / 2);
            let Some(base) = name_score.max(kw_score) else {
                continue;
            };
            let name_lower = e.name.to_lowercase();
            let mut score = base;
            if name_lower.starts_with(&q_lower) {
                score += PREFIX_BONUS;
            } else if name_lower
                .split(|c: char| !c.is_alphanumeric())
                .any(|w| w.starts_with(&q_lower))
            {
                score += WORD_PREFIX_BONUS;
            }
            score += history.boost(&e.target, now);
            hits.push(Hit { entry: e, score });
        }
        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.entry.name.len().cmp(&b.entry.name.len()))
        });
        hits.truncate(limit);
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{AppSource, LaunchTarget};

    fn cat(names: &[&str]) -> Catalog {
        Catalog::build(names.iter().map(|n| AppEntry {
            name: (*n).into(),
            target: LaunchTarget::AppsFolder((*n).into()),
            source: AppSource::StartMenu,
            keywords: vec![],
        }))
    }

    fn names(h: &[Hit<'_>]) -> Vec<String> {
        h.iter().map(|h| h.entry.name.clone()).collect()
    }

    #[test]
    fn prefix_ranks_first() {
        let c = cat(&["Notepad", "Snipping Tool", "Notepad++", "Paint"]);
        let h = Searcher::new().search(&c, &LaunchHistory::default(), "note", 10, 0);
        assert_eq!(names(&h)[0], "Notepad");
        assert!(names(&h).contains(&"Notepad++".to_string()));
        assert!(!names(&h).contains(&"Paint".to_string()));
    }

    #[test]
    fn fuzzy_and_case_insensitive() {
        let c = cat(&["Visual Studio Code", "Windows Terminal"]);
        let h = Searcher::new().search(&c, &LaunchHistory::default(), "vsc", 10, 0);
        assert_eq!(names(&h), vec!["Visual Studio Code"]);
        let h = Searcher::new().search(&c, &LaunchHistory::default(), "TERM", 10, 0);
        assert_eq!(names(&h), vec!["Windows Terminal"]);
    }

    #[test]
    fn word_prefix_beats_scattered_match() {
        let c = cat(&["Terminal", "Task Manager"]);
        let h = Searcher::new().search(&c, &LaunchHistory::default(), "man", 10, 0);
        assert_eq!(names(&h)[0], "Task Manager");
    }

    #[test]
    fn history_breaks_ties_and_orders_empty_query() {
        let c = cat(&["Alpha", "Beta", "Gamma"]);
        let mut hist = LaunchHistory::default();
        let now = 10_000_000;
        for _ in 0..5 {
            hist.record(&LaunchTarget::AppsFolder("Gamma".into()), now);
        }
        let h = Searcher::new().search(&c, &hist, "", 2, now);
        assert_eq!(names(&h), vec!["Gamma", "Alpha"]);
    }

    #[test]
    fn keywords_match_at_lower_weight() {
        let c = Catalog::build([AppEntry {
            name: "Command Prompt".into(),
            target: LaunchTarget::AppsFolder("cmd".into()),
            source: AppSource::StartMenu,
            keywords: vec!["System Tools".into()],
        }]);
        let h = Searcher::new().search(&c, &LaunchHistory::default(), "system", 10, 0);
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn limit_respected() {
        let c = cat(&["a1", "a2", "a3", "a4"]);
        assert_eq!(
            Searcher::new()
                .search(&c, &LaunchHistory::default(), "a", 2, 0)
                .len(),
            2
        );
    }
}
