// SPDX-License-Identifier: GPL-3.0-or-later
//! Background threads of the taskbar. Nothing that can block (icon
//! extraction, Shell enumeration, WASAPI, WMI, network queries) runs on the
//! UI thread; results come back through channels plus a posted message.
//!
//! - [`ShellWorker`] (COM STA): window/app icons, the `shell:AppsFolder`
//!   catalog with resolved identities (for pins and app names), `.lnk`
//!   target resolution.
//! - [`SysWorker`] (COM MTA): volume, mute, output device, battery,
//!   network and brightness; polled (2 s normally, 500 ms while Quick
//!   settings is open; network every 5 s; brightness every 10 s or 500 ms
//!   while open), plus set commands.

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use explorer_core::catalog::{AppEntry, LaunchTarget};
use explorer_core::image::SharedImage;
use explorer_core::shellpath::{looks_like_path, KnownFolders};
use explorer_win::quick_settings::{AudioEndpoint, Battery, Network};
use explorer_win::util::{u64_to_hwnd, ComApartment};
use explorer_win::windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use explorer_win::windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the controller when a [`ShellWorker`] result is ready.
pub const WM_APP_SHELL_DONE: u32 = WM_APP + 20;
/// Posted to the controller when [`SysStatus`] changed.
pub const WM_APP_SYS_CHANGED: u32 = WM_APP + 21;

/// Icons are fetched once at this size and scaled when drawn.
pub const ICON_PX: u32 = 48;

fn post(target: usize, msg: u32) {
    // SAFETY: posting to our own controller window (may be gone at exit).
    let _ = unsafe { PostMessageW(Some(HWND(target as *mut _)), msg, WPARAM(0), LPARAM(0)) };
}

/// A catalog entry with the identity used for matching windows.
#[derive(Debug, Clone)]
pub struct CatalogApp {
    pub entry: AppEntry,
    /// AppUserModelID (AppsFolder id that is not a path).
    pub app_id: Option<String>,
    /// Executable the entry starts, if known.
    pub exe: Option<String>,
}

pub enum ShellJob {
    WindowIcon(u64),
    TargetIcon(String, LaunchTarget),
    Catalog,
    Links(Vec<String>),
}

pub enum ShellDone {
    WindowIcon(u64, Option<SharedImage>),
    TargetIcon(String, Option<SharedImage>),
    Catalog(Vec<CatalogApp>),
    Links(Vec<(String, Option<String>)>),
}

pub struct ShellWorker {
    tx: Sender<ShellJob>,
    pub rx: Receiver<ShellDone>,
}

/// Executable behind a path: `.exe` itself, or a `.lnk` target that is an
/// `.exe`.
fn exe_of(path: &str) -> Option<String> {
    let l = path.to_ascii_lowercase();
    if l.ends_with(".exe") {
        Some(path.to_string())
    } else if l.ends_with(".lnk") {
        explorer_win::appident::shortcut_target(path)
            .filter(|t| t.to_ascii_lowercase().ends_with(".exe"))
    } else {
        None
    }
}

fn identify(e: AppEntry, folders: &KnownFolders) -> CatalogApp {
    let (app_id, exe) = match &e.target {
        LaunchTarget::AppsFolder(id) if looks_like_path(id) => (None, exe_of(&folders.expand(id))),
        LaunchTarget::AppsFolder(id) => (
            Some(id.clone()),
            explorer_win::appident::apps_folder_target(id)
                .filter(|t| t.to_ascii_lowercase().ends_with(".exe")),
        ),
        LaunchTarget::Path(p) => (None, exe_of(&p.to_string_lossy())),
    };
    CatalogApp {
        entry: e,
        app_id,
        exe,
    }
}

impl ShellWorker {
    pub fn start(notify: HWND, folders: KnownFolders) -> Self {
        let (tx, jobs) = channel::<ShellJob>();
        let (done_tx, rx) = channel::<ShellDone>();
        let target = notify.0 as usize;
        let spawned = std::thread::Builder::new()
            .name("taskbar-shell".into())
            .spawn(move || {
                let _com = ComApartment::sta();
                while let Ok(job) = jobs.recv() {
                    let done = match job {
                        ShellJob::WindowIcon(h) => ShellDone::WindowIcon(
                            h,
                            explorer_win::icons::for_window(u64_to_hwnd(h), ICON_PX),
                        ),
                        ShellJob::TargetIcon(k, t) => {
                            ShellDone::TargetIcon(k, explorer_win::icons::for_target(&t, ICON_PX))
                        }
                        ShellJob::Catalog => {
                            let started = Instant::now();
                            let entries = match explorer_win::apps::apps_folder() {
                                Ok(v) => v,
                                Err(e) => {
                                    tracing::warn!(error = %e, "shell:AppsFolder enumeration failed");
                                    Vec::new()
                                }
                            };
                            let apps: Vec<CatalogApp> =
                                entries.into_iter().map(|e| identify(e, &folders)).collect();
                            tracing::info!(
                                apps = apps.len(),
                                ms = started.elapsed().as_millis() as u64,
                                "taskbar catalog ready"
                            );
                            ShellDone::Catalog(apps)
                        }
                        ShellJob::Links(paths) => ShellDone::Links(
                            paths
                                .into_iter()
                                .map(|p| {
                                    let t = exe_of(&p);
                                    (p, t)
                                })
                                .collect(),
                        ),
                    };
                    if done_tx.send(done).is_err() {
                        break;
                    }
                    post(target, WM_APP_SHELL_DONE);
                }
            });
        if let Err(e) = spawned {
            tracing::error!(error = %e, "cannot start the shell worker thread");
        }
        Self { tx, rx }
    }

    pub fn send(&self, job: ShellJob) {
        let _ = self.tx.send(job);
    }
}

/// Launch an app on a short-lived thread (COM STA), like the Start menu.
pub fn launch(target: LaunchTarget) {
    let r = std::thread::Builder::new()
        .name("taskbar-launch".into())
        .spawn(move || {
            let _com = ComApartment::sta();
            tracing::info!(?target, "launching from the taskbar");
            if let Err(e) = explorer_win::apps::launch(&target, false) {
                tracing::error!(error = %e, "launch failed");
            }
        });
    if let Err(e) = r {
        tracing::error!(error = %e, "cannot spawn launch thread");
    }
}

/// Open an `ms-settings:` page (or any shell target) off the UI thread.
pub fn open_uri(uri: &'static str) {
    let r = std::thread::Builder::new()
        .name("taskbar-open".into())
        .spawn(move || {
            let _com = ComApartment::sta();
            if let Err(e) = explorer_win::apps::shell_execute(uri, false) {
                tracing::warn!(error = %e, uri, "cannot open");
            }
        });
    if let Err(e) = r {
        tracing::error!(error = %e, "cannot spawn open thread");
    }
}

// ---------------------------------------------------------------------------
// System status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioStatus {
    pub level: f32,
    pub muted: bool,
    pub device: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SysStatus {
    /// None until the first poll or when there is no output device.
    pub audio: Option<AudioStatus>,
    pub battery: Option<Battery>,
    pub network: Option<Network>,
    /// Built-in panel brightness in percent; None when not supported.
    pub brightness: Option<u8>,
}

pub enum SysCmd {
    SetVolume(f32),
    SetMute(bool),
    SetBrightness(u8),
    /// Poll quickly (Quick settings open) or slowly.
    Fast(bool),
    Refresh,
}

pub struct SysWorker {
    tx: Sender<SysCmd>,
    pub status: Arc<Mutex<SysStatus>>,
}

const SLOW: Duration = Duration::from_secs(2);
const FAST: Duration = Duration::from_millis(500);
const NETWORK_EVERY: Duration = Duration::from_secs(5);
const BRIGHTNESS_EVERY: Duration = Duration::from_secs(10);

struct Poller {
    audio: Option<AudioEndpoint>,
    audio_retry: Option<Instant>,
    brightness: Option<explorer_win::brightness::Brightness>,
    brightness_supported: bool,
    last_network: Option<Instant>,
    last_brightness: Option<Instant>,
}

impl Poller {
    fn audio(&mut self) -> Option<&AudioEndpoint> {
        if self.audio.is_none() && self.audio_retry.is_none_or(|t| t.elapsed() > SLOW * 2) {
            self.audio_retry = Some(Instant::now());
            match AudioEndpoint::open() {
                Ok(a) => self.audio = Some(a),
                Err(e) => tracing::debug!(error = %e, "no default audio endpoint"),
            }
        }
        self.audio.as_ref()
    }

    fn poll(&mut self, st: &mut SysStatus, fast: bool) {
        st.audio = match self.audio().map(|a| a.read()) {
            Some(Ok(a)) => Some(AudioStatus {
                level: a.level,
                muted: a.muted,
                device: a.device,
            }),
            Some(Err(_)) => {
                // Default device changed or vanished: reopen next time.
                self.audio = None;
                None
            }
            None => None,
        };
        // Device switches keep the old endpoint valid; reopen periodically
        // so the name and level follow the new default device.
        if self
            .audio_retry
            .is_some_and(|t| t.elapsed() > Duration::from_secs(10))
        {
            self.audio = None;
        }
        st.battery = explorer_win::quick_settings::battery();
        if self
            .last_network
            .is_none_or(|t| t.elapsed() >= NETWORK_EVERY)
        {
            self.last_network = Some(Instant::now());
            st.network = explorer_win::quick_settings::network().ok();
        }
        let every = if fast { FAST } else { BRIGHTNESS_EVERY };
        if self.brightness_supported && self.last_brightness.is_none_or(|t| t.elapsed() >= every) {
            self.last_brightness = Some(Instant::now());
            if self.brightness.is_none() {
                match explorer_win::brightness::Brightness::open() {
                    Ok(b) => self.brightness = Some(b),
                    Err(e) => {
                        tracing::info!(error = %e, "WMI unavailable; no brightness slider");
                        self.brightness_supported = false;
                    }
                }
            }
            st.brightness = match self.brightness.as_ref().map(|b| b.get()) {
                Some(Ok(Some(v))) => Some(v),
                Some(Ok(None)) => {
                    tracing::info!("no WMI-controllable panel; no brightness slider");
                    self.brightness_supported = false;
                    None
                }
                Some(Err(e)) => {
                    tracing::debug!(error = %e, "brightness query failed");
                    st.brightness
                }
                None => None,
            };
        }
    }
}

impl SysWorker {
    pub fn start(notify: HWND) -> Self {
        let (tx, rx) = channel::<SysCmd>();
        let status = Arc::new(Mutex::new(SysStatus::default()));
        let shared = status.clone();
        let target = notify.0 as usize;
        let spawned = std::thread::Builder::new()
            .name("taskbar-sys".into())
            .spawn(move || {
                let _com = ComApartment::mta();
                let mut p = Poller {
                    audio: None,
                    audio_retry: None,
                    brightness: None,
                    brightness_supported: true,
                    last_network: None,
                    last_brightness: None,
                };
                let mut fast = false;
                let mut last = SysStatus::default();
                let mut first = true;
                loop {
                    let wait = if fast { FAST } else { SLOW };
                    match rx.recv_timeout(if first { Duration::ZERO } else { wait }) {
                        Ok(SysCmd::SetVolume(v)) => {
                            if let Some(a) = p.audio() {
                                let _ = a.set_level(v);
                            }
                        }
                        Ok(SysCmd::SetMute(m)) => {
                            if let Some(a) = p.audio() {
                                let _ = a.set_mute(m);
                            }
                        }
                        Ok(SysCmd::SetBrightness(v)) => {
                            if let Some(b) = p.brightness.as_ref() {
                                if let Err(e) = b.set(v) {
                                    tracing::warn!(error = %e, "cannot set brightness");
                                }
                            }
                            p.last_brightness = None;
                        }
                        Ok(SysCmd::Fast(f)) => {
                            fast = f;
                            p.last_brightness = None;
                        }
                        Ok(SysCmd::Refresh) => {
                            p.last_network = None;
                            p.last_brightness = None;
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                    first = false;
                    let mut st = last.clone();
                    p.poll(&mut st, fast);
                    if st != last {
                        last = st.clone();
                        if let Ok(mut g) = shared.lock() {
                            *g = st;
                        }
                        post(target, WM_APP_SYS_CHANGED);
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::error!(error = %e, "cannot start the system status thread");
        }
        Self { tx, status }
    }

    pub fn send(&self, cmd: SysCmd) {
        let _ = self.tx.send(cmd);
    }

    pub fn snapshot(&self) -> SysStatus {
        self.status.lock().map(|g| g.clone()).unwrap_or_default()
    }
}
