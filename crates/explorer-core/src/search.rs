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
/// Up to this much extra when the query covers most of the word it
/// prefixes: "blo" prefers "Bloc de notas" (3 of 4 letters) to
/// "Blockbench" (3 of 10).
const COVERAGE_BONUS: f32 = 12.0;

/// Share (0..=1) of the first word starting with `q` that `q` covers.
fn word_coverage(name: &str, q: &str) -> f32 {
    let n = fold(name);
    let qn = q.chars().count();
    if qn == 0 {
        return 0.0;
    }
    let mut rest = n.as_str();
    loop {
        if rest.starts_with(q) {
            let word = rest
                .split(|c: char| !c.is_alphanumeric())
                .next()
                .map_or(0, |w| w.chars().count());
            return (qn as f32 / word.max(1) as f32).min(1.0);
        }
        // Next word start.
        let Some(i) = rest.find(|c: char| !c.is_alphanumeric()) else {
            return 0.0;
        };
        rest = rest[i..].trim_start_matches(|c: char| !c.is_alphanumeric());
        if rest.is_empty() {
            return 0.0;
        }
    }
}

/// Lower-case `s` and strip the accents of Latin letters (`Configuración`
/// -> `configuracion`), so prefix bonuses work whichever way the user types.
pub fn fold(s: &str) -> String {
    s.chars().flat_map(fold_char).collect()
}

/// [`fold`] for one character (lower-casing can yield several).
fn fold_char(c: char) -> impl Iterator<Item = char> {
    c.to_lowercase().map(|c| match c {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ñ' => 'n',
        'ç' => 'c',
        c => c,
    })
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

/// Folded characters of `s` and, for each, whether it starts a word
/// (start of text, after a non-alphanumeric character, or a lower → upper
/// case change as in "PowerShell").
fn folded_with_starts(s: &str) -> (Vec<char>, Vec<bool>) {
    let mut chars = Vec::with_capacity(s.len());
    let mut starts = Vec::with_capacity(s.len());
    let mut prev: Option<char> = None;
    for c in s.chars() {
        let start = match prev {
            None => true,
            Some(p) => !p.is_alphanumeric() || (p.is_lowercase() && c.is_uppercase()),
        };
        for (i, f) in fold_char(c).enumerate() {
            chars.push(f);
            starts.push(start && i == 0);
        }
        prev = Some(c);
    }
    (chars, starts)
}

/// Whether a scattered match of `q` in `name` is meaningful: the query
/// must start at a word start and each following character must either
/// continue the previous match or start a new word ("vsc" -> "Visual
/// Studio Code", "wterm" -> "Windows Terminal"), unless the name simply
/// contains the query. This drops noise such as "blo" matching
/// "Compro*b*ar Actua*l*izaci*o*nes".
fn acceptable(name: &str, q: &[char]) -> bool {
    if q.is_empty() {
        return true;
    }
    let (n, starts) = folded_with_starts(name);
    if n.windows(q.len()).any(|w| w == q) {
        return true;
    }
    if q.len() > 32 {
        return false;
    }
    // Depth-first search; names are short and the branching is limited to
    // word starts, so this stays tiny.
    fn go(n: &[char], starts: &[bool], q: &[char], qi: usize, from: usize, prev: usize) -> bool {
        if qi == q.len() {
            return true;
        }
        let mut p = from;
        while p < n.len() {
            if n[p] == q[qi]
                && (starts[p] || (qi > 0 && p == prev + 1))
                && go(n, starts, q, qi + 1, p + 1, p)
            {
                return true;
            }
            p += 1;
        }
        false
    }
    go(&n, &starts, q, 0, 0, 0)
}

/// A parsed, non-empty query, reusable across many candidates.
pub struct Query {
    pattern: Pattern,
    folded: String,
    /// Folded query without whitespace (for [`acceptable`]).
    chars: Vec<char>,
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
            chars: fold(query).chars().filter(|c| !c.is_whitespace()).collect(),
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
            .score(Utf32Str::new(name, &mut self.buf), &mut self.matcher)
            .filter(|_| self.accept(q, name));
        let mut kw_prefix = false;
        let mut kw_score: Option<u32> = None;
        for k in keywords {
            if let Some(s) = q
                .pattern
                .score(Utf32Str::new(k, &mut self.buf), &mut self.matcher)
                .filter(|_| self.accept(q, k))
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
            MatchQuality::Prefix => {
                score += PREFIX_BONUS + (COVERAGE_BONUS * word_coverage(name, &q.folded)) as u32
            }
            MatchQuality::WordPrefix => {
                score +=
                    WORD_PREFIX_BONUS + (COVERAGE_BONUS * word_coverage(name, &q.folded)) as u32
            }
            MatchQuality::Fuzzy if kw_prefix => score += WORD_PREFIX_BONUS,
            MatchQuality::Fuzzy => {}
        }
        Some(score)
    }

    /// Multi-word queries ("panel control") are matched per word by
    /// nucleo; the scattered-match filter applies to single words only.
    fn accept(&self, q: &Query, candidate: &str) -> bool {
        q.folded.contains(char::is_whitespace) || acceptable(candidate, &q.chars)
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
    fn scattered_matches_are_dropped() {
        let c = cat(&[
            "Blockbench",
            "Bloc de notas",
            "Roblox",
            "Comprobar Actualizaciones",
            "Editar las variables de entorno",
            "Propiedades de la tableta Wacom",
            "Subtítulos en directo",
        ]);
        let h = Searcher::new().search(&c, &LaunchHistory::default(), "blo", 10, 0);
        assert_eq!(names(&h)[0], "Bloc de notas", "covers more of its word");
        let mut n = names(&h);
        n.sort();
        assert_eq!(n, vec!["Bloc de notas", "Blockbench", "Roblox"]);
        // Acronyms, camel case and word-start chains still match.
        assert!(acceptable("Visual Studio Code", &['v', 's', 'c']));
        assert!(acceptable(
            "Windows Terminal",
            &"wterm".chars().collect::<Vec<_>>()
        ));
        assert!(acceptable("PowerShell", &['p', 's']));
        assert!(acceptable(
            "Configuración",
            &"guracion".chars().collect::<Vec<_>>()
        ));
        assert!(!acceptable("Terminal", &['m', 'a', 'n']));
        assert!(!acceptable("Paint", &['n', 't', 'p']));
    }

    #[test]
    fn coverage_of_the_prefixed_word() {
        assert_eq!(word_coverage("Bloc de notas", "blo"), 0.75);
        assert_eq!(word_coverage("Blockbench", "blo"), 0.3);
        assert_eq!(word_coverage("Task Manager", "man"), 3.0 / 7.0);
        assert_eq!(word_coverage("Panel de control", "panel de"), 1.0);
        assert_eq!(word_coverage("Paint", "x"), 0.0);
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
