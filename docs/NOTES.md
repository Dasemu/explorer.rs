# Notes

Research notes and open questions. Everything here is **unverified** unless
stated otherwise.

## 1. Investigation: toast notifications without explorer.exe

Status: investigation only, nothing implemented.

What we believe (unverified):

- Toast notifications are rendered by shell UI components (ShellExperienceHost,
  "Windows Shell Experience Host", and on Windows 11 the Notification Center as
  part of the shell UI). With explorer.exe replaced, toasts probably do not
  display, or display inconsistently.
- Apps can probably still post notifications through the ToastNotificationManager
  APIs and the notification platform will probably still accept them. They may
  accumulate with nothing showing them.
- The user-mode notification listener API (`UserNotificationListener` in
  `Windows.UI.Notifications.Management`) could in theory let a custom shell
  read notifications and render its own UI. It requires the
  `userNotificationListener` capability and likely package identity, which a
  plain unpackaged Win32 binary does not have. Whether sparse packaging or
  another route works is unknown.

Experiments to run (in a VM, shell mode):

1. Send a toast from PowerShell or a small test app with explorer.exe not
   running. Does anything display? Does it appear in any queue?
2. Check whether ShellExperienceHost.exe is running, and whether it stays alive
   without explorer.exe.
3. Call `UserNotificationListener.RequestAccessAsync` from an unpackaged app and
   from a sparse-package app. Record the result.
4. If access is granted: read `GetNotificationsAsync` and see which fields are
   usable for rendering.
5. Check whether Focus Assist / Do Not Disturb state is readable without the shell.
6. Check whether apps behave when no toast surface exists (hangs, errors).

## 2. Reference projects and licenses

| Project | URL | License (as we understand it) |
|---|---|---|
| RetroBar | github.com/dremin/RetroBar | Apache-2.0 |
| Cairo Shell | github.com/cairoshell/cairoshell | Apache-2.0 |
| ManagedShell (Cairo's library) | github.com/cairoshell/ManagedShell | Apache-2.0 |

**Verify before relying on this.** Check each repository's LICENSE file
yourself; this table is from memory.

Apache-2.0 is one-way compatible with GPL-3.0: Apache-2.0 code may be included
in a GPL-3.0 work, but not the reverse.

**Project policy:** these projects are used only as *behavioral references*
(for example, which Win32 messages a shell must handle). No code is copied or
translated from them. Implementations here are written independently against
Microsoft's documentation and observed behavior.

## 3. Other shell replacement facts

All from general knowledge and unverified on this project's target builds:

- **TaskbarCreated:** `RegisterWindowMessage("TaskbarCreated")` is broadcast by
  the shell when its taskbar window is created. Tray-icon apps listen for it
  and re-add their icons. A shell that hosts a tray must broadcast it, and
  should do so again after the taskbar is restarted.
- **Shell_TrayWnd:** many apps look up a top-level window of class
  `Shell_TrayWnd` (and children such as `TrayNotifyWnd`) to find the taskbar
  or the tray area. Replacements often create windows with these class names.
- **SetShellWindow / SetTaskmanWindow:** these user32 functions designate the
  shell and task-manager windows. `SetShellWindow` is documented but
  restricted in practice; `SetTaskmanWindow` is undocumented. Their behavior
  (for example, receiving shell hook messages) varies by Windows build.
- **Shell hook messages:** `RegisterShellHookWindow` delivers events such as
  window created / destroyed / activated. This is the documented route for
  taskbar button tracking.
- **AppBar API:** `SHAppBarMessage` (`ABM_NEW`, `ABM_SETPOS`, `ABM_REMOVE`)
  reserves screen edges. Releasing it on exit matters; a crash should release it
  too, which is part of the dev-mode test checklist.
- **Winlogon Shell value:** the shell is read from
  `Winlogon\Shell`. HKCU is documented as per-user override in practice, but
  its handling in Safe Mode is unverified (see RECOVERY.md).
- **Start menu / Search / Widgets:** these are provided by separate processes
  (StartMenuExperienceHost, SearchHost, and so on) that may depend on
  explorer.exe. The launcher in this project replaces them rather than
  hosting them.
