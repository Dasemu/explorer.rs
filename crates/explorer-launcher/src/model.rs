// SPDX-License-Identifier: GPL-3.0-or-later
//! Platform-independent model of the Start menu: result rows and how a
//! query turns into sectioned results. Compiled (and unit-tested) on every
//! platform; the Windows window code in `app.rs` only draws these rows and
//! runs their actions.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use explorer_core::calc;
use explorer_core::catalog::{is_aumid, AppEntry, Catalog, LaunchHistory, LaunchTarget};
use explorer_core::i18n::{tr, Lang};
use explorer_core::recent::{relative_time, RecentFile};
use explorer_core::results::{assemble, ActionContext, Limits, ResultKind, Scored, Section};
use explorer_core::run_command::{RunCommand, RunKind};
use explorer_core::search::{fold, match_quality, MatchQuality, Searcher};
use explorer_core::settings_pages::{self, EntryKind, SettingsEntry};

/// What a row opens.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    App(LaunchTarget),
    Setting(&'static SettingsEntry),
    File {
        path: PathBuf,
        /// Shortcut in the Recent folder, for Recommended items.
        recent_shortcut: Option<PathBuf>,
    },
    Command(RunCommand),
    /// Formatted calculator result (what is copied).
    Calc(String),
}

/// One visible entry: a pin, an app, a result, a recent file.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub name: String,
    /// Second line: kind, folder or time.
    pub detail: String,
    pub kind: ResultKind,
    pub target: Target,
}

impl Row {
    pub fn app(e: &AppEntry, lang: Lang) -> Self {
        Self {
            name: e.name.clone(),
            detail: ResultKind::App.label(lang).to_string(),
            kind: ResultKind::App,
            target: Target::App(e.target.clone()),
        }
    }

    pub fn file(path: PathBuf) -> Self {
        Self {
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned()),
            detail: path
                .parent()
                .map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_default(),
            kind: ResultKind::File,
            target: Target::File {
                path,
                recent_shortcut: None,
            },
        }
    }

    pub fn recent(f: &RecentFile, lang: Lang, now_local: i64, utc_offset: i64) -> Self {
        Self {
            name: f.name.clone(),
            detail: relative_time(lang, f.opened + utc_offset, now_local),
            kind: ResultKind::File,
            target: Target::File {
                path: f.path.clone(),
                recent_shortcut: Some(f.shortcut.clone()),
            },
        }
    }

    pub fn setting(e: &'static SettingsEntry, lang: Lang) -> Self {
        let kind = match e.kind {
            EntryKind::Settings => ResultKind::Setting,
            EntryKind::Tool => ResultKind::Tool,
        };
        Self {
            name: e.name(lang).to_string(),
            detail: kind.label(lang).to_string(),
            kind,
            target: Target::Setting(e),
        }
    }

    pub fn command(c: RunCommand, lang: Lang) -> Self {
        let (name, detail) = match c.kind {
            RunKind::Shell => (
                format!("> {}", c.program),
                tr(
                    lang,
                    "Run in Command Prompt",
                    "Ejecutar en el símbolo del sistema",
                ),
            ),
            RunKind::Path => (c.display(), tr(lang, "Open location", "Abrir ubicación")),
            RunKind::Program => (c.display(), tr(lang, "Run command", "Ejecutar comando")),
        };
        Self {
            name,
            detail: detail.to_string(),
            kind: ResultKind::Command,
            target: Target::Command(c),
        }
    }

    pub fn calc(value: f64, expression: &str, lang: Lang) -> Self {
        let decimal = if lang == Lang::Es { ',' } else { '.' };
        let text = calc::format_result(value, decimal);
        Self {
            name: format!("= {text}"),
            detail: expression.trim().to_string(),
            kind: ResultKind::Calc,
            target: Target::Calc(text),
        }
    }

    /// Key of the cached icon, if the row has a shell icon.
    pub fn icon_key(&self) -> Option<String> {
        self.launch_target().map(|t| t.key())
    }

    /// Target for icon extraction, launch history and pins.
    pub fn launch_target(&self) -> Option<LaunchTarget> {
        match &self.target {
            Target::App(t) => Some(t.clone()),
            Target::File { path, .. } => Some(LaunchTarget::Path(path.clone())),
            _ => None,
        }
    }

    /// File-system location for "Open file location".
    pub fn location(&self) -> Option<&Path> {
        match &self.target {
            Target::App(LaunchTarget::Path(p)) => Some(p),
            Target::File { path, .. } => Some(path),
            _ => None,
        }
    }

    /// Facts for [`explorer_core::results::actions`]. Pin states are
    /// supplied by the caller.
    pub fn action_context(&self, pinned_start: bool, pinned_taskbar: bool) -> ActionContext {
        let packaged =
            matches!(&self.target, Target::App(LaunchTarget::AppsFolder(id)) if is_aumid(id));
        ActionContext {
            elevatable: match &self.target {
                Target::App(_) => !packaged,
                Target::Setting(e) => e.kind == EntryKind::Tool,
                Target::Command(c) => c.kind != RunKind::Path,
                _ => false,
            },
            has_location: self.location().is_some(),
            pinnable: matches!(self.target, Target::App(_)),
            pinned_start,
            pinned_taskbar,
            uninstallable: matches!(self.target, Target::App(_)),
            recent: matches!(
                &self.target,
                Target::File {
                    recent_shortcut: Some(_),
                    ..
                }
            ),
        }
    }
}

/// Everything a search needs besides the query.
pub struct SearchInput<'a> {
    pub query: &'a str,
    pub lang: Lang,
    pub catalog: &'a Catalog,
    pub history: &'a LaunchHistory,
    pub now: u64,
    /// File results (from the index / Everything), best first.
    pub files: &'a [PathBuf],
    /// A command to offer (already resolved when it names a program).
    pub command: Option<RunCommand>,
    /// Filter for settings-table entries (e.g. Terminal only if installed).
    pub tool_available: &'a dyn Fn(&SettingsEntry) -> bool,
    pub limits: Limits,
}

/// Run a query over apps, settings, files, commands and the calculator.
pub fn search(searcher: &mut Searcher, input: &SearchInput<'_>) -> Vec<(Section, Row)> {
    let q = input.query.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let lang = input.lang;
    let mut cands: Vec<Scored<Row>> = Vec::new();

    if let Some(v) = calc::evaluate_query(q) {
        cands.push(Scored {
            kind: ResultKind::Calc,
            score: u32::MAX,
            quality: MatchQuality::Exact,
            item: Row::calc(v, q, lang),
        });
    }

    let app_hits = searcher.search(
        input.catalog,
        input.history,
        q,
        input.limits.apps + 1,
        input.now,
    );
    let app_names: HashSet<String> = app_hits.iter().map(|h| fold(&h.entry.name)).collect();
    for h in &app_hits {
        cands.push(Scored {
            kind: ResultKind::App,
            score: h.score,
            quality: match_quality(&h.entry.name, q),
            item: Row::app(h.entry, lang),
        });
    }

    for h in settings_pages::search(
        searcher,
        q,
        lang,
        input.tool_available,
        input.limits.settings + 2,
    ) {
        let e = h.entry();
        // A tool that is also an installed app ("Command Prompt") shows once.
        if e.kind == EntryKind::Tool
            && (app_names.contains(&fold(e.name_en)) || app_names.contains(&fold(e.name_es)))
        {
            continue;
        }
        let row = Row::setting(e, lang);
        cands.push(Scored {
            kind: row.kind,
            score: h.score,
            quality: h.quality,
            item: row,
        });
    }

    for p in input.files.iter().take(input.limits.documents) {
        let row = Row::file(p.clone());
        cands.push(Scored {
            kind: ResultKind::File,
            score: 0,
            quality: match_quality(&row.name, q),
            item: row,
        });
    }

    if let Some(c) = input.command.clone() {
        cands.push(Scored {
            kind: ResultKind::Command,
            score: 0,
            quality: MatchQuality::Fuzzy,
            item: Row::command(c, lang),
        });
    }

    assemble(cands, &input.limits)
        .into_iter()
        .map(|(section, c)| (section, c.item))
        .collect()
}

/// Rows of the Recommended section: recent files first, then frequently
/// used apps, at most `max`.
pub fn recommended(
    lang: Lang,
    recent: &[RecentFile],
    used: &[&AppEntry],
    now_local: i64,
    utc_offset: i64,
    max: usize,
) -> Vec<Row> {
    let mut rows: Vec<Row> = recent
        .iter()
        .take(max)
        .map(|f| Row::recent(f, lang, now_local, utc_offset))
        .collect();
    for e in used.iter().take(max.saturating_sub(rows.len())) {
        let mut r = Row::app(e, lang);
        r.detail = tr(lang, "Frequently used", "Usada con frecuencia").to_string();
        rows.push(r);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use explorer_core::catalog::AppSource;
    use explorer_core::run_command::classify;

    fn catalog() -> Catalog {
        let app = |n: &str, id: &str| AppEntry {
            name: n.into(),
            target: LaunchTarget::AppsFolder(id.into()),
            source: AppSource::AppsFolder,
            keywords: vec![],
        };
        Catalog::build([
            app(
                "Bloc de notas",
                "Microsoft.WindowsNotepad_8wekyb3d8bbwe!App",
            ),
            app("Símbolo del sistema", "{1AC14E77}\\cmd.exe"),
            app("Paint", "Microsoft.Paint_8wekyb3d8bbwe!App"),
            app("Pantallazo", "Foo_1!App"),
        ])
    }

    fn run(
        query: &str,
        lang: Lang,
        files: &[PathBuf],
        cmd: Option<RunCommand>,
    ) -> Vec<(Section, Row)> {
        let c = catalog();
        let h = LaunchHistory::default();
        let all = |_: &SettingsEntry| true;
        search(
            &mut Searcher::new(),
            &SearchInput {
                query,
                lang,
                catalog: &c,
                history: &h,
                now: 0,
                files,
                command: cmd,
                tool_available: &all,
                limits: Limits::default(),
            },
        )
    }

    #[test]
    fn calculator_is_best_match() {
        let r = run("6*7", Lang::Es, &[], None);
        assert_eq!(r[0].0, Section::BestMatch);
        assert_eq!(r[0].1.name, "= 42");
        let r = run("1/4", Lang::Es, &[], None);
        assert_eq!(r[0].1.target, Target::Calc("0,25".into()));
    }

    #[test]
    fn prefix_app_then_settings() {
        let r = run("panta", Lang::Es, &[], None);
        assert_eq!(r[0].1.name, "Pantallazo");
        assert!(r
            .iter()
            .any(|(s, row)| *s == Section::Settings && row.name == "Pantalla"));
        let r = run("pantalla", Lang::Es, &[], None);
        // "Pantalla" (setting) is an exact match but apps with a prefix
        // match win the Best match, like Windows.
        assert_eq!(r[0].1.name, "Pantallazo");
    }

    #[test]
    fn tools_duplicating_apps_are_hidden() {
        let r = run("simbolo del sistema", Lang::Es, &[], None);
        let n = r
            .iter()
            .filter(|(_, row)| row.name == "Símbolo del sistema")
            .count();
        assert_eq!(n, 1);
        assert_eq!(r[0].1.kind, ResultKind::App);
        // Device Manager is not an app: shown as a tool.
        let r = run("administrador de dispositivos", Lang::Es, &[], None);
        assert_eq!(r[0].1.kind, ResultKind::Tool);
        assert_eq!(r[0].1.detail, "Herramienta del sistema");
    }

    #[test]
    fn files_and_commands_sections() {
        let files = vec![PathBuf::from("C:/docs/informe.docx")];
        let r = run("informe", Lang::En, &files, None);
        assert_eq!(r[0].1.name, "informe.docx");
        assert_eq!(r[0].1.detail, "C:/docs");
        let r = run(">ipconfig", Lang::En, &[], classify(">ipconfig"));
        assert_eq!(r[0].1.name, "> ipconfig");
        assert_eq!(r[0].1.kind, ResultKind::Command);
        assert!(run("   ", Lang::En, &[], None).is_empty());
    }

    #[test]
    fn row_facts() {
        let c = catalog();
        let notepad = Row::app(&c.entries()[0], Lang::En);
        let ctx = notepad.action_context(false, true);
        assert!(!ctx.elevatable); // packaged
        assert!(ctx.pinnable && ctx.pinned_taskbar && ctx.uninstallable);
        assert!(notepad.icon_key().is_some());
        let f = RecentFile {
            name: "a.txt".into(),
            path: PathBuf::from("C:/a.txt"),
            shortcut: PathBuf::from("C:/r/a.txt.lnk"),
            opened: 0,
        };
        let r = Row::recent(&f, Lang::Es, 120, 0);
        assert_eq!(r.detail, "Hace 2 min");
        assert!(r.action_context(false, false).recent);
        assert_eq!(r.location(), Some(Path::new("C:/a.txt")));
        let calc = Row::calc(2.5, " 5/2 ", Lang::En);
        assert_eq!(calc.name, "= 2.5");
        assert_eq!(calc.detail, "5/2");
        assert!(calc.icon_key().is_none());
    }

    #[test]
    fn recommended_fills_with_apps() {
        let c = catalog();
        let used: Vec<&AppEntry> = c.entries().iter().collect();
        let f = RecentFile {
            name: "a.txt".into(),
            path: PathBuf::from("C:/a.txt"),
            shortcut: PathBuf::from("C:/r/a.txt.lnk"),
            opened: 0,
        };
        let r = recommended(Lang::En, &[f], &used, 30, 0, 3);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].detail, "Just now");
        assert_eq!(r[1].detail, "Frequently used");
        assert!(recommended(Lang::En, &[], &[], 0, 0, 6).is_empty());
    }
}
