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
/// Extra bonus when the whole name equals the query (accent-insensitive).
const EXACT_BONUS: u32 = 100;

/// Lower-case `s` and strip the accents of Latin letters (`Configuración`
/// -> `configuracion`), so prefix bonuses work whichever way the user types.
pub fn fold(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            c => c,
        })
        .collect()
}

/// How a candidate's name relates to the query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchQuality {
    Fuzzy,
    WordPrefix,
    Prefix,
    Exact,
}

/// Classify `name` against a query (both folded internally).
pub fn match_quality(name: &str, query: &str) -> MatchQuality {
    let n = fold(name);
    let q = fold(query.trim());
    if q.is_empty() {
        return MatchQuality::Fuzzy;
    }
    if n == q {
        MatchQuality::Exact
    } else if n.starts_with(&q) {
        MatchQuality::Prefix
    } else if n
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w.starts_with(&q))
    {
        MatchQuality::WordPrefix
    } else {
        MatchQuality::Fuzzy
    }
}

/// A parsed, non-empty query, reusable across many candidates.
pub struct Query {
    pattern: Pattern,
    folded: String,
}

impl Query {
    /// `None` for an empty (all-whitespace) query.
    pub fn new(query: &str) -> Option<Self> {
        let query = query.trim();
        if query.is_empty() {
            return None;
        }
        Some(Self {
            pattern: Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart),
            folded: fold(query),
        })
    }
}

impl Searcher {
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
            buf: Vec::new(),
        }
    }

    /// Score one candidate: fuzzy score on the name (keywords at half
    /// weight) plus exact/prefix bonuses. `None` if nothing matches.
    pub fn score<'k>(
        &mut self,
        q: &Query,
        name: &str,
        keywords: impl IntoIterator<Item = &'k str>,
    ) -> Option<u32> {
        let name_score = q
            .pattern
            .score(Utf32Str::new(name, &mut self.buf), &mut self.matcher);
        let mut kw_prefix = false;
        let mut kw_score: Option<u32> = None;
        for k in keywords {
            if let Some(s) = q
                .pattern
                .score(Utf32Str::new(k, &mut self.buf), &mut self.matcher)
            {
                kw_score = Some(kw_score.map_or(s / 2, |b| b.max(s / 2)));
                if !kw_prefix && fold(k).starts_with(&q.folded) {
                    kw_prefix = true;
                }
            }
        }
        let base = name_score.max(kw_score)?;
        let mut score = base;
        match match_quality(name, &q.folded) {
            MatchQuality::Exact => score += EXACT_BONUS + PREFIX_BONUS,
            MatchQuality::Prefix => score += PREFIX_BONUS,
            MatchQuality::WordPrefix => score += WORD_PREFIX_BONUS,
            MatchQuality::Fuzzy if kw_prefix => score += WORD_PREFIX_BONUS,
            MatchQuality::Fuzzy => {}
        }
        Some(score)
    }

    pub fn search<'a>(
        &mut self,
        catalog: &'a Catalog,
        history: &LaunchHistory,
        query: &str,
        limit: usize,
        now: u64,
    ) -> Vec<Hit<'a>> {
        let Some(q) = Query::new(query) else {
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
        };

        let mut hits = Vec::new();
        for e in catalog.entries() {
            let Some(score) = self.score(&q, &e.name, e.keywords.iter().map(String::as_str)) else {
                continue;
            };
            hits.push(Hit {
                entry: e,
                score: score + history.boost(&e.target, now),
            });
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
    fn exact_and_accent_insensitive_prefix() {
        let c = cat(&["Configuración", "Configurar copia", "Mapa"]);
        let h = Searcher::new().search(&c, &LaunchHistory::default(), "configuracion", 10, 0);
        assert_eq!(names(&h)[0], "Configuración");
        assert_eq!(
            match_quality("Configuración", "CONFIG"),
            MatchQuality::Prefix
        );
        assert_eq!(match_quality("Mapa", "mapa"), MatchQuality::Exact);
        assert_eq!(
            match_quality("Task Manager", "man"),
            MatchQuality::WordPrefix
        );
        assert_eq!(match_quality("Paint", "pnt"), MatchQuality::Fuzzy);
        assert_eq!(fold("ÑANDÚ"), "nandu");
    }

    #[test]
    fn keyword_prefix_gets_a_bonus() {
        let mut s = Searcher::new();
        let q = Query::new("wifi").unwrap();
        let with = s.score(&q, "Network", ["wifi"]).unwrap();
        let without = s.score(&q, "Network", ["xwifi"]);
        assert!(without.is_none_or(|w| w < with));
        assert!(Query::new("  ").is_none());
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
