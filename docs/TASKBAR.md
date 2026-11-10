# Taskbar

The taskbar (`explorer-rs-taskbar`) aims at Windows 11 parity where it
matters. Nothing in this document is verified on real Windows yet; see the
last section.

## Layout

```text
[Start][pinned + running apps...][…]        [^][tray][net vol bat][clock][|]
```

- Start button and app buttons are centred or left-aligned
  (`taskbar.alignment`, default follows `TaskbarAl`).
- Buttons shrink to a minimum width; those that still do not fit go behind
  the **"…"** button, which opens a list of the hidden apps (icon + name).
- **`^`** (only when some tray icons are hidden) opens the tray overflow grid.
- **System glyphs** (network, volume, battery if present) open Quick settings.
- **Clock** opens the calendar.
- The thin strip at the far right is **Show desktop**.

Geometry is pure and tested: `explorer_core::taskbar_layout`,
`explorer_core::flyout_layout`.

## App buttons

- **Grouping** (`taskbar.combine = "always"`, default): one button per app.
  Group key = the window's AppUserModelID if it has one, else the process
  image path (case-insensitive), else the window itself.
  `combine = "never"`: one button per window (labels optional with
  `button_labels`).
- **Pins** come first, in pin order (`%LOCALAPPDATA%\explorer-rs\taskbar-pins.toml`,
  shared with the Start menu, re-read when its modification time changes;
  seeded once with `DEFAULT_TASKBAR_PIN_IDS` when the app catalog is first
  available). The catalog is `shell:AppsFolder`, enumerated on a worker
  thread and refreshed every 15 minutes; each entry gets an identity
  (AUMID, executable via `PKEY_Link_TargetParsingPath` / `.lnk` target /
  known-folder expansion).
- **Pin ↔ window matching** (`taskbar_groups::match_pin`): AUMID equality
  first (two different AUMIDs never match, so Edge web apps stay separate),
  then executable path equality, then — only without conflicting AUMIDs —
  case-insensitive display-name equality with the exe stem or window title.
  A group goes to its strongest-matching pin.
- **Indicators**: short dim bar = running, long accent bar = has the active
  window, amber background = flashing.
- **Click**: not running → launch; one window → activate, or minimize if it
  is already active; several windows → sticky window picker.
  **Shift+click / middle-click** → new instance.
- **Drag & drop** of pinned buttons (combine = always): press on a pinned
  button and move > 6 px; an accent caret shows the drop position; release
  persists the order (`reorder_pins`; uninstalled pins keep their place).
- **Context menu** (bilingual): app name (new instance), Pin/Unpin,
  Close window / Close all windows (n). Empty area: Task Manager, Taskbar
  settings, Restart taskbar, Quit explorer.rs (dev) / Sign out (shell).
  Clock and system glyphs add a settings shortcut at the top.

## Window picker (thumbnails)

- Hovering a running app for 400 ms shows the picker above the button
  ("peek", never activated): one card per window with icon, title, close
  (×) on hover, and a live DWM thumbnail (`DwmRegisterThumbnail`). Moving to
  another running button switches immediately. It hides 300 ms after the
  pointer left both button and popup (polled every 100 ms).
- Clicking a multi-window app opens it **sticky** (activated; closes on
  click-away or Esc). Clicking a card activates that window.
- Minimized windows: DWM reports only the minimized caption as source size
  (Windows' own taskbar asks apps for iconic bitmaps), so cards whose source
  is smaller than 64×48 show the app icon and title instead.

## Tray

- Only **promoted** icons are on the bar; the rest are behind `^`.
  Promotion comes from `HKCU\Control Panel\NotifyIconSettings\<id>`
  (`ExecutablePath` expanded from known-folder GUIDs, `UID`, `IsPromoted`),
  matched by owner process image path, then `UID` when an app has several
  entries (`tray_promotion::lookup`). No entry → overflow (Windows default).
  Re-read every 5 s. `tray.show_all = true` puts every icon on the bar.
- In the overflow grid, left click, double click and right click are
  forwarded exactly like on the bar (`TrayHost::click`); right click closes
  the flyout first so the app's menu is not covered.

## Quick settings

Volume slider + mute (click the speaker) + output device (click: Sound
settings); brightness slider when WMI `WmiMonitorBrightness` reports an
internal panel; network name and status with "Network & internet settings";
battery percentage and state with "Power & battery settings"; Settings
button. Mouse wheel over the bar's volume glyph or the flyout changes the
volume by 2 % per notch.

Back-ends (`explorer_win::quick_settings`, `explorer_win::brightness`) run on
a worker thread (COM MTA) that **polls**: every 2 s, every 500 ms while
Quick settings is open; network every 5 s; brightness every 10 s (500 ms
while open). `WM_POWERBROADCAST` triggers an immediate refresh. Change
callbacks (`IAudioEndpointVolumeCallback`, `INetworkListManagerEvents`)
would need COM object implementations and were not worth it for a glyph.
Wi-Fi signal strength is not read (the glyph shows full bars).

## Calendar

Full date ("martes, 4 de noviembre de 2026"), month title, previous/next
(also mouse wheel, PageUp/PageDown, arrow keys), today highlighted; click the
title to return to today. First weekday from `LOCALE_IFIRSTDAYOFWEEK`
(Monday for Spanish, Sunday for en-US); names from our bilingual tables
(`explorer_core::calendar`).

## Show desktop

Click: minimize every non-minimized taskbar window (remembered, Z order);
click again: restore them and activate the previous top window. Activating
a taskbar window more than 1.5 s after the minimize forgets the toggle.

## Flyout infrastructure

One window (`ExplorerRs.TaskbarFlyout`, topmost tool window, rounded corners
via `DWMWA_WINDOW_CORNER_PREFERENCE`, D2D-painted with the palette) reused
for every popup; state lives in `Shared::flyout`. Placement:
`flyout_layout::place` above (or below) the bar on the opener's monitor,
scaled by that monitor's DPI. Sticky flyouts call `force_foreground` (keyboard
focus, Esc closes) and close on `WM_ACTIVATE(WA_INACTIVE)`; as a safety net a
sticky flyout that is not the foreground window 700 ms after opening is
closed. The bar keeps `MA_NOACTIVATE`, so clicking the opener toggles the
flyout closed; a 400 ms debounce after a deactivation-close prevents
reopening (same idea as the Start menu's `TOGGLE_DEBOUNCE`).

## Threads

UI thread: windows, model, painting. `taskbar-shell` (COM STA): window and
app icons, `shell:AppsFolder` catalog + identities, `.lnk` resolution.
`taskbar-sys` (COM MTA): audio, battery, network, WMI. Launches and
`ms-settings:` URIs run on short-lived threads. Window identities
(`SHGetPropertyStoreForWindow`, `QueryFullProcessImageNameW`) are read on the
UI thread; neither sends messages to the target window.

## Known limitations / TODO

- No jump lists, no "recent" items in the context menu.
- No Wi-Fi signal strength, no Wi-Fi/Bluetooth/airplane toggles, no
  external-monitor (DDC/CI) brightness.
- In shell mode nobody creates `NotifyIconSettings` entries for new apps, so
  their icons stay in the overflow until promoted in Settings (if Settings
  can do so without explorer) or `tray.show_all = true`.
- Path pins that are not in `shell:AppsFolder` show the file stem as name.
- Dragging unpinned buttons and dragging in `combine = "never"` is not
  supported.
- No drop shadow on flyouts (window class without `CS_DROPSHADOW`).

## For the lead

Merged into ARCHITECTURE.md, STATUS.md and TESTING-ON-WINDOWS.md.
