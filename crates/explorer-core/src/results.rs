// SPDX-License-Identifier: GPL-3.0-or-later
//! Start menu search results: sections, the "Best match", the actions of
//! the detail panel and keyboard focus movement.
//!
//! Everything here is generic over the payload so the launcher keeps its
//! own row type and the rules stay testable on Linux:
//! - [`assemble`] orders scored candidates into Windows-like sections:
//!   Best match, Apps, Settings, Documents, Commands;
//! - [`actions`] lists what the detail panel / context menu offers;
//! - [`arrow`] and [`tab`] move keyboard focus between "zones" (pinned grid,
//!   result list, detail actions, footer buttons...).

use crate::i18n::{tr, Lang};
use crate::search::MatchQuality;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Section {
    BestMatch,
    Apps,
    Settings,
    Documents,
    Commands,
}

impl Section {
    pub fn title(self, lang: Lang) -> &'static str {
        match self {
            Section::BestMatch => tr(lang, "Best match", "Mejor coincidencia"),
            Section::Apps => tr(lang, "Apps", "Aplicaciones"),
            Section::Settings => tr(lang, "Settings", "Configuración"),
            Section::Documents => tr(lang, "Documents", "Documentos"),
            Section::Commands => tr(lang, "Commands", "Comandos"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResultKind {
    /// Installed app (Start menu shortcut or packaged app).
    App,
    /// Classic system tool from the settings table (Device Manager...).
    Tool,
    /// Windows Settings page.
    Setting,
    File,
    Folder,
    /// "Run ..." (program, path or `>` shell command).
    Command,
    /// Calculator result.
    Calc,
}

impl ResultKind {
    /// Kind label shown under the name ("App", "System settings"...).
    pub fn label(self, lang: Lang) -> &'static str {
        match self {
            ResultKind::App => tr(lang, "App", "Aplicación"),
            ResultKind::Tool => tr(lang, "System tool", "Herramienta del sistema"),
            ResultKind::Setting => tr(lang, "System settings", "Configuración del sistema"),
            ResultKind::File => tr(lang, "File", "Archivo"),
            ResultKind::Folder => tr(lang, "Folder", "Carpeta"),
            ResultKind::Command => tr(lang, "Command", "Comando"),
            ResultKind::Calc => tr(lang, "Calculator", "Calculadora"),
        }
    }

    pub fn section(self) -> Section {
        match self {
            ResultKind::App | ResultKind::Tool => Section::Apps,
            ResultKind::Setting => Section::Settings,
            ResultKind::File | ResultKind::Folder => Section::Documents,
            ResultKind::Command | ResultKind::Calc => Section::Commands,
        }
    }
}

/// A candidate result before sectioning.
#[derive(Debug, Clone, PartialEq)]
pub struct Scored<T> {
    pub kind: ResultKind,
    pub score: u32,
    /// How the name matched the query (decides the Best match).
    pub quality: MatchQuality,
    pub item: T,
}

/// Maximum number of rows per section (Best match excluded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub apps: usize,
    pub settings: usize,
    pub documents: usize,
    pub commands: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            apps: 8,
            settings: 4,
            documents: 5,
            commands: 3,
        }
    }
}

fn limit_of(l: &Limits, s: Section) -> usize {
    match s {
        Section::BestMatch => 1,
        Section::Apps => l.apps,
        Section::Settings => l.settings,
        Section::Documents => l.documents,
        Section::Commands => l.commands,
    }
}

/// Pick the Best match:
/// 1. a calculator result;
/// 2. the app/tool whose name equals or starts with the query (best
///    quality, then score);
/// 3. the highest-scoring app, tool or setting with a word-prefix match;
/// 4. a document whose name starts with the query;
/// 5. the highest-scoring app, tool or setting (fuzzy match);
/// 6. a "Run" command, then any document.
fn best_index<T>(c: &[Scored<T>]) -> Option<usize> {
    let pick = |pred: &dyn Fn(&Scored<T>) -> bool| -> Option<usize> {
        c.iter()
            .enumerate()
            .filter(|(_, s)| pred(s))
            .max_by(|(ia, a), (ib, b)| {
                (a.quality, a.score)
                    .cmp(&(b.quality, b.score))
                    // Earlier candidate wins ties.
                    .then_with(|| ib.cmp(ia))
            })
            .map(|(i, _)| i)
    };
    let by_score = |pred: &dyn Fn(&Scored<T>) -> bool| -> Option<usize> {
        c.iter()
            .enumerate()
            .filter(|(_, s)| pred(s))
            .max_by(|(ia, a), (ib, b)| a.score.cmp(&b.score).then_with(|| ib.cmp(ia)))
            .map(|(i, _)| i)
    };
    pick(&|s| s.kind == ResultKind::Calc)
        .or_else(|| {
            pick(&|s| {
                matches!(s.kind, ResultKind::App | ResultKind::Tool)
                    && s.quality >= MatchQuality::Prefix
            })
        })
        .or_else(|| {
            by_score(&|s| {
                matches!(
                    s.kind,
                    ResultKind::App | ResultKind::Tool | ResultKind::Setting
                ) && s.quality >= MatchQuality::WordPrefix
            })
        })
        .or_else(|| {
            pick(&|s| {
                matches!(s.kind, ResultKind::File | ResultKind::Folder)
                    && s.quality >= MatchQuality::Prefix
            })
        })
        .or_else(|| {
            by_score(&|s| {
                matches!(
                    s.kind,
                    ResultKind::App | ResultKind::Tool | ResultKind::Setting
                )
            })
        })
        .or_else(|| by_score(&|s| s.kind == ResultKind::Command))
        .or_else(|| by_score(&|s| matches!(s.kind, ResultKind::File | ResultKind::Folder)))
}

/// Order candidates into sections. The result is flat: each row carries its
/// section; a header is drawn wherever the section changes.
pub fn assemble<T>(mut candidates: Vec<Scored<T>>, limits: &Limits) -> Vec<(Section, Scored<T>)> {
    let mut out = Vec::with_capacity(candidates.len());
    if let Some(b) = best_index(&candidates) {
        out.push((Section::BestMatch, candidates.remove(b)));
    }
    // Stable sort: equal scores keep the caller's order.
    candidates.sort_by(|a, b| {
        a.kind
            .section()
            .cmp(&b.kind.section())
            .then_with(|| b.score.cmp(&a.score))
    });
    let mut count = 0;
    let mut current = None;
    for c in candidates {
        let s = c.kind.section();
        if current != Some(s) {
            current = Some(s);
            count = 0;
        }
        if count < limit_of(limits, s) {
            count += 1;
            out.push((s, c));
        }
    }
    out
}

/// Indexes of the rows that start a new section (where a header goes).
pub fn header_rows<T>(rows: &[(Section, T)]) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter(|(i, (s, _))| *i == 0 || rows[i - 1].0 != *s)
        .map(|(i, _)| i)
        .collect()
}

/// Something the detail panel or a context menu can do with a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Open,
    RunAsAdmin,
    OpenLocation,
    CopyPath,
    CopyResult,
    PinStart,
    UnpinStart,
    PinTaskbar,
    UnpinTaskbar,
    Uninstall,
    /// Remove a recent file from the Recommended list.
    RemoveFromList,
}

impl Action {
    pub fn label(self, kind: ResultKind, lang: Lang) -> &'static str {
        match self {
            Action::Open => match kind {
                ResultKind::Command => tr(lang, "Run", "Ejecutar"),
                ResultKind::Calc => tr(lang, "Copy", "Copiar"),
                _ => tr(lang, "Open", "Abrir"),
            },
            Action::RunAsAdmin => tr(lang, "Run as administrator", "Ejecutar como administrador"),
            Action::OpenLocation => tr(lang, "Open file location", "Abrir ubicación del archivo"),
            Action::CopyPath => tr(lang, "Copy path", "Copiar ruta de acceso"),
            Action::CopyResult => tr(lang, "Copy", "Copiar"),
            Action::PinStart => tr(lang, "Pin to Start", "Anclar a Inicio"),
            Action::UnpinStart => tr(lang, "Unpin from Start", "Desanclar de Inicio"),
            Action::PinTaskbar => tr(lang, "Pin to taskbar", "Anclar a la barra de tareas"),
            Action::UnpinTaskbar => tr(
                lang,
                "Unpin from taskbar",
                "Desanclar de la barra de tareas",
            ),
            Action::Uninstall => tr(lang, "Uninstall", "Desinstalar"),
            Action::RemoveFromList => tr(lang, "Remove from list", "Quitar de la lista"),
        }
    }
}

/// What is known about a result when listing its actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ActionContext {
    /// May be started elevated (classic apps, tools, commands).
    pub elevatable: bool,
    /// Has a file-system location to show in File Explorer.
    pub has_location: bool,
    /// Can be pinned (catalog apps only).
    pub pinnable: bool,
    pub pinned_start: bool,
    pub pinned_taskbar: bool,
    /// Offer "Uninstall" (installed apps, not system tools).
    pub uninstallable: bool,
    /// Item of the Recommended list.
    pub recent: bool,
}

/// Actions for a result, in display order. The first is the default
/// (Enter / click).
pub fn actions(kind: ResultKind, ctx: &ActionContext) -> Vec<Action> {
    let mut v = vec![Action::Open];
    match kind {
        ResultKind::Calc => return v,
        ResultKind::Setting => return v,
        ResultKind::Command => {
            if ctx.elevatable {
                v.push(Action::RunAsAdmin);
            }
            return v;
        }
        ResultKind::File | ResultKind::Folder => {
            if ctx.has_location {
                v.push(Action::OpenLocation);
                v.push(Action::CopyPath);
            }
            if ctx.recent {
                v.push(Action::RemoveFromList);
            }
            return v;
        }
        ResultKind::App | ResultKind::Tool => {}
    }
    if ctx.elevatable {
        v.push(Action::RunAsAdmin);
    }
    if ctx.has_location {
        v.push(Action::OpenLocation);
    }
    if ctx.pinnable {
        v.push(if ctx.pinned_start {
            Action::UnpinStart
        } else {
            Action::PinStart
        });
        v.push(if ctx.pinned_taskbar {
            Action::UnpinTaskbar
        } else {
            Action::PinTaskbar
        });
    }
    if ctx.uninstallable && kind == ResultKind::App {
        v.push(Action::Uninstall);
    }
    v
}

/// Layout of a focus zone, for arrow keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneKind {
    /// Row-major grid with `cols` columns.
    Grid { cols: usize },
    /// Vertical list.
    List,
    /// Horizontal row of buttons.
    Row,
}

/// A group of focusable items. Tab moves between zones (or between the
/// items of a `tab_each` zone); arrows move inside a zone and, when
/// `arrows_leave`, out of it into the neighbouring zone. A `contained`
/// zone (`arrows_leave == false`) is reached with Tab only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zone {
    pub kind: ZoneKind,
    pub len: usize,
    pub tab_each: bool,
    pub arrows_leave: bool,
}

impl Zone {
    pub fn new(kind: ZoneKind, len: usize) -> Self {
        Self {
            kind,
            len,
            tab_each: false,
            arrows_leave: true,
        }
    }

    pub fn tab_each(mut self) -> Self {
        self.tab_each = true;
        self
    }

    pub fn contained(mut self) -> Self {
        self.arrows_leave = false;
        self
    }
}

/// `(zone, item)`; `None` means the search box has focus.
pub type Focus = Option<(usize, usize)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrow {
    Up,
    Down,
    Left,
    Right,
}

fn next_zone(zones: &[Zone], from: usize) -> Option<usize> {
    (from + 1..zones.len()).find(|&z| zones[z].len > 0)
}

fn prev_zone(zones: &[Zone], from: usize) -> Option<usize> {
    (0..from).rev().find(|&z| zones[z].len > 0)
}

fn first_zone(zones: &[Zone]) -> Option<usize> {
    zones.iter().position(|z| z.len > 0)
}

/// Neighbouring zones for arrow keys: only those that take part in arrow
/// navigation (`arrows_leave`).
fn next_arrow_zone(zones: &[Zone], from: usize) -> Option<usize> {
    (from + 1..zones.len()).find(|&z| zones[z].len > 0 && zones[z].arrows_leave)
}

fn prev_arrow_zone(zones: &[Zone], from: usize) -> Option<usize> {
    (0..from)
        .rev()
        .find(|&z| zones[z].len > 0 && zones[z].arrows_leave)
}

/// Move focus with an arrow key.
pub fn arrow(zones: &[Zone], cur: Focus, key: Arrow) -> Focus {
    let Some((z, i)) = cur.filter(|(z, i)| zones.get(*z).is_some_and(|zz| *i < zz.len)) else {
        // From the search box (or a stale focus): Up stays, anything else
        // enters the first zone.
        return match key {
            Arrow::Up => None,
            _ => zones
                .iter()
                .position(|z| z.len > 0 && z.arrows_leave)
                .map(|z| (z, 0)),
        };
    };
    let zone = zones[z];
    enum Out {
        Stay(usize),
        LeaveUp,
        LeaveDown,
        Keep,
    }
    let out = match (zone.kind, key) {
        (ZoneKind::Grid { cols }, k) => {
            let cols = cols.max(1);
            match k {
                Arrow::Left if i > 0 => Out::Stay(i - 1),
                Arrow::Right if i + 1 < zone.len => Out::Stay(i + 1),
                Arrow::Left | Arrow::Right => Out::Keep,
                Arrow::Up if i >= cols => Out::Stay(i - cols),
                Arrow::Up => Out::LeaveUp,
                Arrow::Down if i + cols < zone.len => Out::Stay(i + cols),
                Arrow::Down if i / cols < (zone.len - 1) / cols => Out::Stay(zone.len - 1),
                Arrow::Down => Out::LeaveDown,
            }
        }
        (ZoneKind::List, Arrow::Up) if i > 0 => Out::Stay(i - 1),
        (ZoneKind::List, Arrow::Up) => Out::LeaveUp,
        (ZoneKind::List, Arrow::Down) if i + 1 < zone.len => Out::Stay(i + 1),
        (ZoneKind::List, Arrow::Down) => Out::LeaveDown,
        (ZoneKind::List, _) => Out::Keep,
        (ZoneKind::Row, Arrow::Left) if i > 0 => Out::Stay(i - 1),
        (ZoneKind::Row, Arrow::Right) if i + 1 < zone.len => Out::Stay(i + 1),
        (ZoneKind::Row, Arrow::Left | Arrow::Right) => Out::Keep,
        (ZoneKind::Row, Arrow::Up) => Out::LeaveUp,
        (ZoneKind::Row, Arrow::Down) => Out::LeaveDown,
    };
    match out {
        Out::Stay(n) => Some((z, n)),
        Out::Keep => Some((z, i)),
        Out::LeaveUp if zone.arrows_leave => match prev_arrow_zone(zones, z) {
            Some(p) => {
                let pz = zones[p];
                let idx = match pz.kind {
                    ZoneKind::Row => 0,
                    _ => pz.len - 1,
                };
                Some((p, idx))
            }
            None => None,
        },
        Out::LeaveDown if zone.arrows_leave => match next_arrow_zone(zones, z) {
            Some(n) => Some((n, 0)),
            None => Some((z, i)),
        },
        Out::LeaveUp | Out::LeaveDown => Some((z, i)),
    }
}

/// Move focus with Tab (`forward`) or Shift+Tab. The search box (`None`)
/// is part of the cycle: search box -> zones in order -> search box.
pub fn tab(zones: &[Zone], cur: Focus, forward: bool) -> Focus {
    let cur = cur.filter(|(z, i)| zones.get(*z).is_some_and(|zz| *i < zz.len));
    if forward {
        match cur {
            None => first_zone(zones).map(|z| (z, 0)),
            Some((z, i)) if zones[z].tab_each && i + 1 < zones[z].len => Some((z, i + 1)),
            Some((z, _)) => next_zone(zones, z).map(|n| (n, 0)),
        }
    } else {
        let enter_back = |z: usize| {
            let zz = zones[z];
            Some((z, if zz.tab_each { zz.len - 1 } else { 0 }))
        };
        match cur {
            None => zones.iter().rposition(|z| z.len > 0).and_then(enter_back),
            Some((z, i)) if zones[z].tab_each && i > 0 => Some((z, i - 1)),
            Some((z, _)) => prev_zone(zones, z).and_then(enter_back),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(
        kind: ResultKind,
        score: u32,
        q: MatchQuality,
        name: &'static str,
    ) -> Scored<&'static str> {
        Scored {
            kind,
            score,
            quality: q,
            item: name,
        }
    }

    fn names(v: &[(Section, Scored<&'static str>)]) -> Vec<(Section, &'static str)> {
        v.iter().map(|(s, c)| (*s, c.item)).collect()
    }

    use MatchQuality::*;
    use ResultKind::*;

    #[test]
    fn prefix_app_beats_higher_scoring_setting() {
        let r = assemble(
            vec![
                c(Setting, 300, Fuzzy, "Display"),
                c(App, 120, Prefix, "Discord"),
                c(App, 200, Fuzzy, "Media Player"),
                c(File, 999, Exact, "disc.txt"),
            ],
            &Limits::default(),
        );
        assert_eq!(
            names(&r),
            vec![
                (Section::BestMatch, "Discord"),
                (Section::Apps, "Media Player"),
                (Section::Settings, "Display"),
                (Section::Documents, "disc.txt"),
            ]
        );
        assert_eq!(header_rows(&r), vec![0, 1, 2, 3]);
    }

    #[test]
    fn calc_then_score_then_commands_then_files() {
        let r = assemble(
            vec![c(App, 10, Fuzzy, "x"), c(Calc, 0, Fuzzy, "= 4")],
            &Limits::default(),
        );
        assert_eq!(r[0].1.item, "= 4");
        let r = assemble(
            vec![c(App, 10, Fuzzy, "a"), c(Setting, 50, Fuzzy, "s")],
            &Limits::default(),
        );
        assert_eq!(r[0].1.item, "s");
        let r = assemble(
            vec![c(File, 10, Fuzzy, "f"), c(Command, 1, Fuzzy, "run")],
            &Limits::default(),
        );
        assert_eq!(names(&r)[0], (Section::BestMatch, "run"));
        let r = assemble(vec![c(File, 10, Fuzzy, "f")], &Limits::default());
        assert_eq!(names(&r), vec![(Section::BestMatch, "f")]);
        assert!(assemble::<&str>(vec![], &Limits::default()).is_empty());
    }

    #[test]
    fn document_prefix_beats_fuzzy_setting() {
        let r = assemble(
            vec![
                c(Setting, 90, Fuzzy, "About"),
                c(File, 0, Prefix, "informe.docx"),
            ],
            &Limits::default(),
        );
        assert_eq!(r[0].1.item, "informe.docx");
        let r = assemble(
            vec![
                c(Setting, 90, WordPrefix, "Network"),
                c(File, 0, Prefix, "network.txt"),
            ],
            &Limits::default(),
        );
        assert_eq!(r[0].1.item, "Network");
    }

    #[test]
    fn exact_beats_prefix_and_limits_apply() {
        let l = Limits {
            apps: 2,
            ..Limits::default()
        };
        let r = assemble(
            vec![
                c(App, 500, Prefix, "Paint 3D"),
                c(App, 100, Exact, "Paint"),
                c(App, 90, Fuzzy, "a"),
                c(App, 80, Fuzzy, "b"),
                c(App, 70, Fuzzy, "c"),
            ],
            &l,
        );
        assert_eq!(
            names(&r),
            vec![
                (Section::BestMatch, "Paint"),
                (Section::Apps, "Paint 3D"),
                (Section::Apps, "a"),
            ]
        );
    }

    #[test]
    fn action_lists() {
        let app = ActionContext {
            elevatable: true,
            has_location: true,
            pinnable: true,
            pinned_start: true,
            uninstallable: true,
            ..Default::default()
        };
        assert_eq!(
            actions(App, &app),
            vec![
                Action::Open,
                Action::RunAsAdmin,
                Action::OpenLocation,
                Action::UnpinStart,
                Action::PinTaskbar,
                Action::Uninstall
            ]
        );
        // Tools are never "uninstalled".
        assert!(!actions(Tool, &app).contains(&Action::Uninstall));
        assert_eq!(actions(Calc, &app), vec![Action::Open]);
        assert_eq!(actions(Setting, &app), vec![Action::Open]);
        assert_eq!(
            actions(Command, &app),
            vec![Action::Open, Action::RunAsAdmin]
        );
        let file = ActionContext {
            has_location: true,
            recent: true,
            ..Default::default()
        };
        assert_eq!(
            actions(File, &file),
            vec![
                Action::Open,
                Action::OpenLocation,
                Action::CopyPath,
                Action::RemoveFromList
            ]
        );
        assert_eq!(Action::Open.label(Command, Lang::Es), "Ejecutar");
        assert_eq!(
            Action::PinTaskbar.label(App, Lang::Es),
            "Anclar a la barra de tareas"
        );
        assert_eq!(Section::BestMatch.title(Lang::Es), "Mejor coincidencia");
        assert_eq!(Setting.label(Lang::En), "System settings");
    }

    fn home() -> Vec<Zone> {
        vec![
            Zone::new(ZoneKind::Row, 1),              // "All apps"
            Zone::new(ZoneKind::Grid { cols: 6 }, 8), // pinned
            Zone::new(ZoneKind::Grid { cols: 2 }, 3), // recommended
            Zone::new(ZoneKind::Row, 3).tab_each(),   // footer
        ]
    }

    #[test]
    fn arrows_move_within_and_across_zones() {
        let z = home();
        assert_eq!(arrow(&z, None, Arrow::Down), Some((0, 0)));
        assert_eq!(arrow(&z, None, Arrow::Up), None);
        assert_eq!(arrow(&z, Some((0, 0)), Arrow::Up), None);
        assert_eq!(arrow(&z, Some((0, 0)), Arrow::Down), Some((1, 0)));
        assert_eq!(arrow(&z, Some((1, 0)), Arrow::Down), Some((1, 6)));
        assert_eq!(arrow(&z, Some((1, 3)), Arrow::Down), Some((1, 7))); // partial row
        assert_eq!(arrow(&z, Some((1, 7)), Arrow::Down), Some((2, 0)));
        assert_eq!(arrow(&z, Some((1, 5)), Arrow::Right), Some((1, 6)));
        assert_eq!(arrow(&z, Some((1, 7)), Arrow::Right), Some((1, 7)));
        assert_eq!(arrow(&z, Some((2, 0)), Arrow::Up), Some((1, 7)));
        assert_eq!(arrow(&z, Some((2, 2)), Arrow::Down), Some((3, 0)));
        assert_eq!(arrow(&z, Some((3, 0)), Arrow::Right), Some((3, 1)));
        assert_eq!(arrow(&z, Some((3, 1)), Arrow::Down), Some((3, 1)));
        assert_eq!(arrow(&z, Some((3, 1)), Arrow::Up), Some((2, 2)));
        // Stale focus is repaired.
        assert_eq!(arrow(&z, Some((1, 99)), Arrow::Down), Some((0, 0)));
    }

    #[test]
    fn contained_zones_keep_focus() {
        let z = vec![
            Zone::new(ZoneKind::List, 3),
            Zone::new(ZoneKind::List, 2).tab_each().contained(),
        ];
        assert_eq!(arrow(&z, Some((1, 0)), Arrow::Up), Some((1, 0)));
        assert_eq!(arrow(&z, Some((1, 1)), Arrow::Down), Some((1, 1)));
        // Contained zones are not entered by arrows either (Tab only).
        assert_eq!(arrow(&z, Some((0, 2)), Arrow::Down), Some((0, 2)));
        let z = vec![
            Zone::new(ZoneKind::List, 3),
            Zone::new(ZoneKind::List, 2).contained(),
            Zone::new(ZoneKind::Row, 2),
        ];
        assert_eq!(arrow(&z, Some((0, 2)), Arrow::Down), Some((2, 0)));
        assert_eq!(arrow(&z, Some((2, 1)), Arrow::Up), Some((0, 2)));
        let z = vec![
            Zone::new(ZoneKind::Row, 1).contained(),
            Zone::new(ZoneKind::List, 2),
        ];
        assert_eq!(arrow(&z, None, Arrow::Down), Some((1, 0)));
    }

    #[test]
    fn tab_cycles_through_search_box() {
        let z = vec![
            Zone::new(ZoneKind::List, 3),
            Zone::new(ZoneKind::List, 0),
            Zone::new(ZoneKind::List, 2).tab_each(),
            Zone::new(ZoneKind::Row, 2).tab_each(),
        ];
        let mut f = None;
        let mut seen = vec![];
        for _ in 0..7 {
            f = tab(&z, f, true);
            seen.push(f);
        }
        assert_eq!(
            seen,
            vec![
                Some((0, 0)),
                Some((2, 0)),
                Some((2, 1)),
                Some((3, 0)),
                Some((3, 1)),
                None,
                Some((0, 0)),
            ]
        );
        assert_eq!(tab(&z, None, false), Some((3, 1)));
        assert_eq!(tab(&z, Some((3, 0)), false), Some((2, 1)));
        assert_eq!(tab(&z, Some((2, 0)), false), Some((0, 0)));
        assert_eq!(tab(&z, Some((0, 2)), false), None);
        assert_eq!(tab(&[], None, true), None);
    }
}
