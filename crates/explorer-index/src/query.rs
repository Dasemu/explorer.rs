// SPDX-License-Identifier: GPL-3.0-or-later
//! Query language and evaluation.
//!
//! A query is a list of whitespace-separated terms that must all match
//! (AND). Double quotes group text containing spaces. Terms:
//!
//! | term          | matches                                              |
//! |---------------|------------------------------------------------------|
//! | `foo`         | name contains `foo` (case-insensitive)               |
//! | `^foo`        | name starts with `foo`                               |
//! | `foo$`        | name ends with `foo`; `^foo$` is an exact name       |
//! | `~foo`        | fuzzy match on the name (`nucleo-matcher`)           |
//! | `src\main`    | a term with `\` or `/` matches against the full path |
//! | `path:foo`    | full path contains `foo`                             |
//! | `dir:foo`     | some parent folder's path contains `foo`             |
//! | `dir:` / `folder:` | folders only                                    |
//! | `file:`       | files only                                           |
//! | `ext:rs;toml` | file extension is one of the list                    |
//! | `size:>10mb`  | size comparison (`>`, `>=`, `<`, `<=`, `=`; b/kb/mb/gb/tb) |
//! | `!term`       | negation of any of the above                         |
//!
//! Evaluation: the longest positive substring/prefix/suffix term drives a
//! single `memmem` pass over the folded name arena; every candidate is then
//! checked against the remaining terms. Path terms are evaluated with a
//! per-query memo over ancestors, so they cost O(entries) at worst.

use std::sync::atomic::{AtomicBool, Ordering};

use memchr::memmem;
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::index::{fold, EntryId, Index, NONE};

/// Longest accepted query text in bytes.
pub const MAX_QUERY_BYTES: usize = 1024;
/// Most terms in one query.
pub const MAX_TERMS: usize = 16;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueryOptions {
    /// Plain terms match against the full path instead of the name.
    pub match_path: bool,
    /// Plain terms are fuzzy (as if prefixed with `~`).
    pub fuzzy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QueryError {
    #[error("query too long")]
    TooLong,
    #[error("too many terms")]
    TooManyTerms,
    #[error("invalid size filter: {0}")]
    BadSize(String),
    #[error("cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cmp {
    Lt,
    Le,
    Eq,
    Ge,
    Gt,
}

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Contains(Vec<u8>),
    Prefix(Vec<u8>),
    Suffix(Vec<u8>),
    Exact(Vec<u8>),
    Fuzzy(String),
    Path(Vec<Vec<u8>>),
    InDir(Vec<Vec<u8>>),
    Ext(Vec<Vec<u8>>),
    DirsOnly,
    FilesOnly,
    Size(Cmp, u64),
}

#[derive(Debug, Clone, PartialEq)]
struct Term {
    neg: bool,
    kind: Kind,
}

/// A parsed query.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    terms: Vec<Term>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub id: EntryId,
    pub score: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchResult {
    /// Number of matching entries (before any access filtering).
    pub total: usize,
    /// Best hits first, at most the requested number.
    pub hits: Vec<Hit>,
}

fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut had_quotes = false;
    for c in s.chars() {
        if c == '"' {
            quoted = !quoted;
            had_quotes = true;
        } else if c.is_whitespace() && !quoted {
            if !cur.is_empty() || had_quotes {
                out.push(std::mem::take(&mut cur));
            }
            had_quotes = false;
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() || had_quotes {
        out.push(cur);
    }
    out.retain(|t| !t.is_empty());
    out
}

fn path_parts(v: &str) -> Vec<Vec<u8>> {
    let f = fold(v);
    let raw: Vec<&str> = f.split(['/', '\\']).collect();
    let n = raw.len();
    raw.iter()
        .enumerate()
        .filter(|(i, p)| !p.is_empty() || *i == 0 || *i == n - 1)
        .map(|(_, p)| p.as_bytes().to_vec())
        .collect()
}

fn parse_size(v: &str) -> Result<(Cmp, u64), QueryError> {
    let bad = || QueryError::BadSize(v.to_string());
    let (cmp, rest) = if let Some(r) = v.strip_prefix(">=") {
        (Cmp::Ge, r)
    } else if let Some(r) = v.strip_prefix("<=") {
        (Cmp::Le, r)
    } else if let Some(r) = v.strip_prefix('>') {
        (Cmp::Gt, r)
    } else if let Some(r) = v.strip_prefix('<') {
        (Cmp::Lt, r)
    } else if let Some(r) = v.strip_prefix('=') {
        (Cmp::Eq, r)
    } else {
        (Cmp::Eq, v)
    };
    let rest = rest.trim().to_ascii_lowercase();
    let digits = rest
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(rest.len());
    let (num, unit) = rest.split_at(digits);
    let n: f64 = num.parse().map_err(|_| bad())?;
    let mul: u64 = match unit.trim() {
        "" | "b" => 1,
        "k" | "kb" => 1 << 10,
        "m" | "mb" => 1 << 20,
        "g" | "gb" => 1 << 30,
        "t" | "tb" => 1 << 40,
        _ => return Err(bad()),
    };
    let bytes = n * mul as f64;
    if !bytes.is_finite() || bytes < 0.0 || bytes > u64::MAX as f64 {
        return Err(bad());
    }
    Ok((cmp, bytes as u64))
}

impl Query {
    pub fn parse(text: &str, opts: QueryOptions) -> Result<Self, QueryError> {
        if text.len() > MAX_QUERY_BYTES {
            return Err(QueryError::TooLong);
        }
        let tokens = tokenize(text);
        if tokens.len() > MAX_TERMS {
            return Err(QueryError::TooManyTerms);
        }
        let mut terms = Vec::with_capacity(tokens.len());
        for tok in tokens {
            let (neg, t) = match tok.strip_prefix('!') {
                Some(r) if !r.is_empty() => (true, r),
                _ => (false, tok.as_str()),
            };
            let kind = if let Some((op, val)) = t.split_once(':') {
                match op.to_ascii_lowercase().as_str() {
                    "ext" => Some(Kind::Ext(
                        val.split([';', ','])
                            .map(|e| fold(e.trim_start_matches('.')).into_bytes())
                            .filter(|e| !e.is_empty())
                            .collect(),
                    )),
                    "path" if !val.is_empty() => Some(Kind::Path(path_parts(val))),
                    "dir" | "folder" if val.is_empty() => Some(Kind::DirsOnly),
                    "dir" | "folder" => Some(Kind::InDir(path_parts(val))),
                    "file" | "files" => Some(Kind::FilesOnly),
                    "size" => {
                        let (c, n) = parse_size(val)?;
                        Some(Kind::Size(c, n))
                    }
                    _ => None,
                }
            } else {
                None
            };
            let kind = match kind {
                Some(k) => k,
                None => plain_kind(t, opts),
            };
            if let Kind::Contains(ref v) | Kind::Prefix(ref v) | Kind::Suffix(ref v) = kind {
                if v.is_empty() {
                    continue;
                }
            }
            terms.push(Term { neg, kind });
        }
        Ok(Self { terms })
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Index of the term that drives the arena scan, if any.
    fn driver(&self) -> Option<usize> {
        self.terms
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.neg)
            .filter_map(|(i, t)| match &t.kind {
                Kind::Contains(v) | Kind::Prefix(v) | Kind::Suffix(v) | Kind::Exact(v) => {
                    Some((i, v.len()))
                }
                _ => None,
            })
            .max_by_key(|&(_, l)| l)
            .map(|(i, _)| i)
    }
}

fn plain_kind(t: &str, opts: QueryOptions) -> Kind {
    if t.contains(['/', '\\']) {
        return Kind::Path(path_parts(t));
    }
    if let Some(r) = t.strip_prefix('~') {
        return Kind::Fuzzy(fold(r));
    }
    let (pre, t) = match t.strip_prefix('^') {
        Some(r) => (true, r),
        None => (false, t),
    };
    let (suf, t) = match t.strip_suffix('$') {
        Some(r) if !r.is_empty() => (true, r),
        _ => (false, t),
    };
    let v = fold(t).into_bytes();
    match (pre, suf) {
        (true, true) => Kind::Exact(v),
        (true, false) => Kind::Prefix(v),
        (false, true) => Kind::Suffix(v),
        (false, false) if opts.fuzzy => Kind::Fuzzy(fold(t)),
        (false, false) if opts.match_path => Kind::Path(vec![v]),
        (false, false) => Kind::Contains(v),
    }
}

fn is_subsequence(needle: &[u8], hay: &[u8]) -> bool {
    let mut it = hay.iter();
    needle.iter().all(|n| it.any(|h| h == n))
}

fn word_boundary(name: &[u8], pos: usize) -> bool {
    pos == 0
        || matches!(
            name[pos - 1],
            b' ' | b'-' | b'_' | b'.' | b'(' | b'[' | b'+' | b','
        )
}

struct Eval<'a> {
    ix: &'a Index,
    q: &'a Query,
    finders: Vec<Option<memmem::Finder<'a>>>,
    memos: Vec<Vec<u8>>,
    atoms: Vec<Option<Atom>>,
    matcher: Matcher,
    buf: Vec<char>,
}

impl<'a> Eval<'a> {
    fn new(ix: &'a Index, q: &'a Query) -> Self {
        let finders = q
            .terms
            .iter()
            .map(|t| match &t.kind {
                Kind::Contains(v) => Some(memmem::Finder::new(v.as_slice())),
                _ => None,
            })
            .collect();
        let atoms = q
            .terms
            .iter()
            .map(|t| match &t.kind {
                Kind::Fuzzy(s) => Some(Atom::new(
                    s,
                    CaseMatching::Ignore,
                    Normalization::Smart,
                    AtomKind::Fuzzy,
                    false,
                )),
                _ => None,
            })
            .collect();
        Self {
            ix,
            q,
            finders,
            memos: vec![Vec::new(); q.terms.len()],
            atoms,
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            buf: Vec::new(),
        }
    }

    /// Does the component chain ending at `e` match `parts`?
    fn window(ix: &Index, parts: &[Vec<u8>], e: EntryId) -> bool {
        let n = parts.len();
        let name = ix.folded_name(e);
        if n == 1 {
            return memmem::find(name, &parts[0]).is_some();
        }
        if !name.starts_with(&parts[n - 1]) {
            return false;
        }
        let mut a = e;
        for part in parts[1..n - 1].iter().rev() {
            a = ix.parent(a);
            if a == NONE || ix.folded_name(a) != part.as_slice() {
                return false;
            }
        }
        a = ix.parent(a);
        a != NONE && ix.folded_name(a).ends_with(&parts[0])
    }

    /// Memoized "path of `id` matches `parts`" (self or any ancestor window).
    fn path_match(&mut self, t: usize, parts: &[Vec<u8>], id: EntryId) -> bool {
        if id == NONE {
            return false;
        }
        let memo = &mut self.memos[t];
        if memo.len() < self.ix.id_bound() {
            memo.resize(self.ix.id_bound(), 0);
        }
        let mut chain = Vec::new();
        let mut a = id;
        let mut v = false;
        while a != NONE {
            match memo[a as usize] {
                1 => {
                    v = true;
                    break;
                }
                2 => break,
                _ => {
                    chain.push(a);
                    a = self.ix.parent(a);
                }
            }
        }
        for &c in chain.iter().rev() {
            v = v || Self::window(self.ix, parts, c);
            memo[c as usize] = if v { 1 } else { 2 };
        }
        v
    }

    /// Score of `id` if it matches every term.
    fn check(&mut self, id: EntryId) -> Option<u32> {
        let ix = self.ix;
        let q = self.q;
        let name = ix.folded_name(id);
        let mut score: u32 = 0;
        for (t, term) in q.terms.iter().enumerate() {
            let (ok, s) = match &term.kind {
                Kind::Contains(v) => match self.finders[t].as_ref().and_then(|f| f.find(name)) {
                    Some(p) if name.len() == v.len() => (true, 1000 + p as u32),
                    Some(0) => (true, 500),
                    Some(p) if word_boundary(name, p) => (true, 200),
                    Some(_) => (true, 100),
                    None => (false, 0),
                },
                Kind::Prefix(v) => {
                    let ok = name.starts_with(v);
                    (ok, if name.len() == v.len() { 1000 } else { 500 })
                }
                Kind::Suffix(v) => (name.ends_with(v), 150),
                Kind::Exact(v) => (name == v.as_slice(), 1000),
                Kind::Fuzzy(s) => {
                    if !is_subsequence(s.as_bytes(), name) {
                        (false, 0)
                    } else {
                        let txt = std::str::from_utf8(name).unwrap_or("");
                        let hay = Utf32Str::new(txt, &mut self.buf);
                        match self.atoms[t]
                            .as_ref()
                            .and_then(|a| a.score(hay, &mut self.matcher))
                        {
                            Some(sc) => (true, sc as u32),
                            None => (false, 0),
                        }
                    }
                }
                Kind::Path(parts) => (self.path_match(t, parts, id), 50),
                Kind::InDir(parts) => (self.path_match(t, parts, ix.parent(id)), 50),
                Kind::Ext(list) => {
                    let ok = !ix.is_dir(id)
                        && memchr::memrchr(b'.', name).is_some_and(|d| {
                            let e = &name[d + 1..];
                            list.iter().any(|x| x.as_slice() == e)
                        });
                    (ok, 0)
                }
                Kind::DirsOnly => (ix.is_dir(id), 0),
                Kind::FilesOnly => (!ix.is_dir(id), 0),
                Kind::Size(c, n) => {
                    let ok = ix.size(id).is_some_and(|s| match c {
                        Cmp::Lt => s < *n,
                        Cmp::Le => s <= *n,
                        Cmp::Eq => s == *n,
                        Cmp::Ge => s >= *n,
                        Cmp::Gt => s > *n,
                    });
                    (ok, 0)
                }
            };
            if ok == term.neg {
                return None;
            }
            if !term.neg {
                score = score.saturating_add(s);
            }
        }
        Some(score)
    }
}

fn sort_key(ix: &Index, id: EntryId, score: u32) -> u64 {
    let len = ix.folded_name(id).len().min(0xFFFF) as u64;
    ((score as u64) << 24) | ((0xFFFF - len) << 8) | u64::from(ix.is_dir(id))
}

/// Evaluate `q`, returning the total match count and the best `want` hits.
pub fn search(
    ix: &Index,
    q: &Query,
    want: usize,
    cancel: &AtomicBool,
) -> Result<SearchResult, QueryError> {
    if q.is_empty() {
        return Ok(SearchResult::default());
    }
    let mut ev = Eval::new(ix, q);
    let mut hits: Vec<(u64, EntryId)> = Vec::new();
    let mut steps: u32 = 0;
    let mut visit = |id: EntryId, ev: &mut Eval<'_>| -> Result<(), QueryError> {
        steps = steps.wrapping_add(1);
        if steps % 8192 == 0 && cancel.load(Ordering::Relaxed) {
            return Err(QueryError::Cancelled);
        }
        if ix.is_root(id) {
            return Ok(());
        }
        if let Some(s) = ev.check(id) {
            hits.push((sort_key(ix, id, s), id));
        }
        Ok(())
    };
    match q.driver() {
        Some(d) => {
            let needle: &[u8] = match &q.terms[d].kind {
                Kind::Contains(v) | Kind::Prefix(v) | Kind::Suffix(v) | Kind::Exact(v) => v,
                _ => unreachable!("driver is a substring term"),
            };
            let finder = memmem::Finder::new(needle);
            let hay = ix.arena();
            let mut from = 0;
            let mut seg = 0;
            while from < hay.len() {
                let Some(p) = finder.find(&hay[from..]) else {
                    break;
                };
                seg = ix.seg_at(from + p, seg);
                let (_, end) = ix.seg_bounds(seg);
                let id = ix.seg_owner(seg);
                if id != NONE {
                    visit(id, &mut ev)?;
                }
                from = end + 1;
            }
        }
        None => {
            for id in ix.live_ids() {
                visit(id, &mut ev)?;
            }
        }
    }
    let total = hits.len();
    let cmp = |a: &(u64, EntryId), b: &(u64, EntryId)| b.0.cmp(&a.0).then(a.1.cmp(&b.1));
    if want == 0 {
        hits.clear();
    } else if hits.len() > want {
        hits.select_nth_unstable_by(want - 1, cmp);
        hits.truncate(want);
    }
    hits.sort_unstable_by(cmp);
    Ok(SearchResult {
        total,
        hits: hits
            .into_iter()
            .map(|(k, id)| Hit {
                id,
                score: (k >> 24) as u32,
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{VolumeKind, SEP};

    fn sample() -> Index {
        let mut ix = Index::new();
        let v = ix.add_volume("C:\\", VolumeKind::Walk).unwrap();
        let root = ix.volume(v).unwrap().root;
        let users = ix.insert(root, "Users", true, None, NONE).unwrap();
        let me = ix.insert(users, "Me", true, None, NONE).unwrap();
        let docs = ix.insert(me, "Documents", true, None, NONE).unwrap();
        let src = ix.insert(me, "src", true, None, NONE).unwrap();
        let proj = ix.insert(src, "explorer.rs", true, None, NONE).unwrap();
        ix.insert(proj, "main.rs", false, Some(1200), NONE).unwrap();
        ix.insert(proj, "Cargo.toml", false, Some(300), NONE)
            .unwrap();
        ix.insert(docs, "Report 2024.docx", false, Some(5 << 20), NONE)
            .unwrap();
        ix.insert(docs, "report-final.PDF", false, Some(10), NONE)
            .unwrap();
        ix.insert(docs, "main.rs.bak", false, None, NONE).unwrap();
        let win = ix.insert(root, "Windows", true, None, NONE).unwrap();
        ix.insert(win, "notepad.exe", false, None, NONE).unwrap();
        ix
    }

    fn names(ix: &Index, q: &str) -> Vec<String> {
        let q = Query::parse(q, QueryOptions::default()).unwrap();
        let r = search(ix, &q, 100, &AtomicBool::new(false)).unwrap();
        assert_eq!(r.total, r.hits.len());
        r.hits.iter().map(|h| ix.name(h.id).into_owned()).collect()
    }

    fn sorted(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    #[test]
    fn tokenizer() {
        assert_eq!(tokenize(r#"a "b c"  d"#), vec!["a", "b c", "d"]);
        assert_eq!(tokenize(r#"path:"x y""#), vec!["path:x y"]);
        assert_eq!(tokenize("   "), Vec::<String>::new());
        assert_eq!(tokenize(r#""""#), Vec::<String>::new());
    }

    #[test]
    fn substring_and_ranking() {
        let ix = sample();
        let r = names(&ix, "MAIN");
        assert_eq!(r, vec!["main.rs", "main.rs.bak"]);
        // Exact name beats prefix beats word boundary beats inside.
        assert_eq!(names(&ix, "report")[0], "Report 2024.docx");
        assert_eq!(sorted(names(&ix, "report")).len(), 2);
        assert!(names(&ix, "zzz").is_empty());
    }

    #[test]
    fn prefix_suffix_exact() {
        let ix = sample();
        assert_eq!(names(&ix, "^rep").len(), 2);
        assert_eq!(names(&ix, "^port").len(), 0);
        assert_eq!(sorted(names(&ix, ".rs$")), vec!["explorer.rs", "main.rs"]);
        assert_eq!(names(&ix, "^main.rs$"), vec!["main.rs"]);
    }

    #[test]
    fn operators() {
        let ix = sample();
        assert_eq!(
            sorted(names(&ix, "ext:rs;toml")),
            vec!["Cargo.toml", "main.rs"]
        );
        assert_eq!(names(&ix, "ext:pdf"), vec!["report-final.PDF"]);
        assert_eq!(names(&ix, "main dir:explorer"), vec!["main.rs"]);
        assert_eq!(names(&ix, "main !dir:explorer"), vec!["main.rs.bak"]);
        assert_eq!(names(&ix, "path:documents main"), vec!["main.rs.bak"]);
        assert_eq!(
            sorted(names(&ix, "dir: me")),
            vec!["Documents".to_string(), "Me".into()]
        );
        assert_eq!(names(&ix, "file: ^not"), vec!["notepad.exe"]);
        assert_eq!(names(&ix, "size:>1mb"), vec!["Report 2024.docx"]);
        assert_eq!(names(&ix, "size:<=300 ext:toml"), vec!["Cargo.toml"]);
        assert!(Query::parse("size:>lots", QueryOptions::default()).is_err());
    }

    #[test]
    fn path_terms_with_separators() {
        let ix = sample();
        assert_eq!(
            sorted(names(&ix, "src/explorer.rs")),
            vec!["Cargo.toml", "explorer.rs", "main.rs"]
        );
        assert_eq!(names(&ix, r"plorer.rs\ma"), vec!["main.rs"]);
        assert_eq!(names(&ix, r"me\src\explorer.rs\main"), vec!["main.rs"]);
        assert!(names(&ix, r"me\explorer.rs").is_empty());
        assert_eq!(names(&ix, r"c:\windows\note"), vec!["notepad.exe"]);
    }

    #[test]
    fn fuzzy() {
        let ix = sample();
        let r = names(&ix, "~rprtfin");
        assert_eq!(r, vec!["report-final.PDF"]);
        let q = Query::parse(
            "ntpd",
            QueryOptions {
                fuzzy: true,
                ..Default::default()
            },
        )
        .unwrap();
        let r = search(&ix, &q, 10, &AtomicBool::new(false)).unwrap();
        assert_eq!(ix.name(r.hits[0].id), "notepad.exe");
    }

    #[test]
    fn match_path_option() {
        let ix = sample();
        let q = Query::parse(
            "windows",
            QueryOptions {
                match_path: true,
                ..Default::default()
            },
        )
        .unwrap();
        let r = search(&ix, &q, 10, &AtomicBool::new(false)).unwrap();
        assert_eq!(r.total, 2);
        let p = ix.path(r.hits.iter().find(|h| !ix.is_dir(h.id)).unwrap().id);
        assert!(p.ends_with(&format!("Windows{SEP}notepad.exe")));
    }

    #[test]
    fn limits_and_cancel() {
        let ix = sample();
        let q = Query::parse("e", QueryOptions::default()).unwrap();
        let r = search(&ix, &q, 2, &AtomicBool::new(false)).unwrap();
        assert!(r.total > 2);
        assert_eq!(r.hits.len(), 2);
        assert!(r.hits[0].score >= r.hits[1].score);
        let r0 = search(&ix, &q, 0, &AtomicBool::new(false)).unwrap();
        assert!(r0.hits.is_empty() && r0.total == r.total);
        assert!(Query::parse(&"a".repeat(MAX_QUERY_BYTES + 1), QueryOptions::default()).is_err());
        let many = vec!["a"; MAX_TERMS + 1].join(" ");
        assert!(Query::parse(&many, QueryOptions::default()).is_err());
        assert!(Query::parse("", QueryOptions::default())
            .unwrap()
            .is_empty());
        assert!(Query::parse("!", QueryOptions::default()).is_ok());
    }

    #[test]
    fn negation_only_scans_everything() {
        let ix = sample();
        let r = names(&ix, "!e");
        assert!(r.contains(&"src".to_string()));
        assert!(!r.iter().any(|n| n.to_lowercase().contains('e')));
    }
}
