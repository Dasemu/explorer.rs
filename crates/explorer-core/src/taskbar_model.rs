// SPDX-License-Identifier: GPL-3.0-or-later
//! Ordered list of taskbar buttons, driven by window events.
//!
//! The Win32 layer translates shell hook / WinEvent notifications into
//! [`WindowEvent`]s and fresh [`WindowInfo`] snapshots; this model keeps the
//! button order stable (new windows are appended, like Windows does) and
//! tracks the active window. It is pure and fully unit-tested.

use crate::window_filter::{FilterRules, WindowInfo};

/// Events produced by the Win32 layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowEvent {
    /// A window appeared or one of its properties changed (title, style,
    /// cloak state...). The snapshot is re-filtered.
    Upsert(WindowInfo),
    /// The window was destroyed.
    Destroyed(u64),
    /// The window became the foreground window.
    Activated(u64),
    /// The window flashes / requests attention (HSHELL_FLASH).
    Flash(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    pub hwnd: u64,
    pub title: String,
    pub process_name: Option<String>,
    pub flashing: bool,
}

#[derive(Debug, Default)]
pub struct TaskbarModel {
    rules: FilterRules,
    buttons: Vec<Button>,
    active: Option<u64>,
}

impl TaskbarModel {
    pub fn new(rules: FilterRules) -> Self {
        Self {
            rules,
            buttons: Vec::new(),
            active: None,
        }
    }

    pub fn buttons(&self) -> &[Button] {
        &self.buttons
    }

    pub fn active(&self) -> Option<u64> {
        self.active
    }

    fn index_of(&self, hwnd: u64) -> Option<usize> {
        self.buttons.iter().position(|b| b.hwnd == hwnd)
    }

    /// Replace the whole list from a full enumeration, keeping the existing
    /// order for windows that are still present. Returns true if changed.
    pub fn reset(&mut self, windows: impl IntoIterator<Item = WindowInfo>) -> bool {
        let shown: Vec<WindowInfo> = windows
            .into_iter()
            .filter(|w| self.rules.should_show(w))
            .collect();
        let mut next: Vec<Button> = self
            .buttons
            .iter()
            .filter_map(|b| shown.iter().find(|w| w.hwnd == b.hwnd).map(|w| (b, w)))
            .map(|(b, w)| Button {
                hwnd: w.hwnd,
                title: w.title.clone(),
                process_name: w.process_name.clone(),
                flashing: b.flashing,
            })
            .collect();
        for w in &shown {
            if !next.iter().any(|b| b.hwnd == w.hwnd) {
                next.push(Button {
                    hwnd: w.hwnd,
                    title: w.title.clone(),
                    process_name: w.process_name.clone(),
                    flashing: false,
                });
            }
        }
        let changed = next != self.buttons;
        self.buttons = next;
        if let Some(a) = self.active {
            if self.index_of(a).is_none() {
                self.active = None;
            }
        }
        changed
    }

    /// Apply one event. Returns true if the visible state changed.
    pub fn apply(&mut self, ev: WindowEvent) -> bool {
        match ev {
            WindowEvent::Upsert(w) => {
                let show = self.rules.should_show(&w);
                match (self.index_of(w.hwnd), show) {
                    (Some(i), true) => {
                        let b = &mut self.buttons[i];
                        let changed = b.title != w.title || b.process_name != w.process_name;
                        b.title = w.title;
                        b.process_name = w.process_name;
                        changed
                    }
                    (Some(i), false) => {
                        self.buttons.remove(i);
                        true
                    }
                    (None, true) => {
                        self.buttons.push(Button {
                            hwnd: w.hwnd,
                            title: w.title,
                            process_name: w.process_name,
                            flashing: false,
                        });
                        true
                    }
                    (None, false) => false,
                }
            }
            WindowEvent::Destroyed(h) => {
                let removed = self.index_of(h).map(|i| self.buttons.remove(i)).is_some();
                if self.active == Some(h) {
                    self.active = None;
                    return true;
                }
                removed
            }
            WindowEvent::Activated(h) => {
                let mut changed = self.active != Some(h);
                self.active = Some(h);
                if let Some(i) = self.index_of(h) {
                    if self.buttons[i].flashing {
                        self.buttons[i].flashing = false;
                        changed = true;
                    }
                }
                changed
            }
            WindowEvent::Flash(h) => match self.index_of(h) {
                Some(i) if self.active != Some(h) && !self.buttons[i].flashing => {
                    self.buttons[i].flashing = true;
                    true
                }
                _ => false,
            },
        }
    }

    /// What clicking a button should do: activate the window, or minimize it
    /// if it is already the active one (standard taskbar behaviour).
    pub fn click_action(&self, hwnd: u64, is_minimized: bool) -> ClickAction {
        if self.active == Some(hwnd) && !is_minimized {
            ClickAction::Minimize
        } else {
            ClickAction::Activate
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickAction {
    Activate,
    Minimize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window_filter::style::*;

    fn win(h: u64, title: &str) -> WindowInfo {
        WindowInfo {
            hwnd: h,
            title: title.into(),
            class_name: "C".into(),
            style: WS_VISIBLE,
            ..Default::default()
        }
    }

    #[test]
    fn order_is_stable_and_appends() {
        let mut m = TaskbarModel::default();
        m.reset([win(1, "a"), win(2, "b")]);
        m.apply(WindowEvent::Upsert(win(3, "c")));
        // A full re-enumeration in a different order must not reshuffle.
        m.reset([win(3, "c"), win(2, "b"), win(1, "a")]);
        let order: Vec<u64> = m.buttons().iter().map(|b| b.hwnd).collect();
        assert_eq!(order, vec![1, 2, 3]);
    }

    #[test]
    fn upsert_filters_and_updates() {
        let mut m = TaskbarModel::default();
        assert!(m.apply(WindowEvent::Upsert(win(1, "a"))));
        assert!(m.apply(WindowEvent::Upsert(win(1, "a2"))));
        assert_eq!(m.buttons()[0].title, "a2");
        assert!(!m.apply(WindowEvent::Upsert(win(1, "a2"))));
        let mut cloaked = win(1, "a2");
        cloaked.cloaked = 1;
        assert!(m.apply(WindowEvent::Upsert(cloaked)));
        assert!(m.buttons().is_empty());
    }

    #[test]
    fn destroy_and_active() {
        let mut m = TaskbarModel::default();
        m.reset([win(1, "a"), win(2, "b")]);
        m.apply(WindowEvent::Activated(2));
        assert_eq!(m.active(), Some(2));
        m.apply(WindowEvent::Destroyed(2));
        assert_eq!(m.active(), None);
        assert_eq!(m.buttons().len(), 1);
    }

    #[test]
    fn flash_cleared_on_activation() {
        let mut m = TaskbarModel::default();
        m.reset([win(1, "a"), win(2, "b")]);
        m.apply(WindowEvent::Activated(1));
        assert!(m.apply(WindowEvent::Flash(2)));
        assert!(m.buttons()[1].flashing);
        // Flashing the active window is ignored.
        assert!(!m.apply(WindowEvent::Flash(1)));
        m.apply(WindowEvent::Activated(2));
        assert!(!m.buttons()[1].flashing);
    }

    #[test]
    fn click_toggles_minimize() {
        let mut m = TaskbarModel::default();
        m.reset([win(1, "a")]);
        assert_eq!(m.click_action(1, false), ClickAction::Activate);
        m.apply(WindowEvent::Activated(1));
        assert_eq!(m.click_action(1, false), ClickAction::Minimize);
        assert_eq!(m.click_action(1, true), ClickAction::Activate);
    }
}
