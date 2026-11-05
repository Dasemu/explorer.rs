# Start menu (`explorer-rs-launcher`)

The Start menu is a single popup window anchored above the taskbar (centred
or left, following `TaskbarAl` / `[taskbar] alignment`). It stays hidden but
warm between uses: the app catalog, icons, pins and recent files remain in
memory, and the first frame is painted synchronously right after the window
is shown. No web results, ever.

Code: `crates/explorer-launcher/src/` (`app.rs` window and painting,
`model.rs` rows and search assembly, `worker.rs` background threads,
`sys.rs` Win32 helpers, `cue.rs` search placeholder, `avatar.rs` round
account picture, `files.rs`/`file_index.rs`/`everything.rs` file results).
Pure logic in `explorer-core`: `search.rs`, `settings_pages.rs`, `calc.rs`,
`run_command.rs`, `recent.rs`, `results.rs`, `start.rs`.

## Language

Every string is bilingual through `explorer_core::i18n::tr`, resolved from
`[general] language` (`system` follows the Windows display language; any
language other than Spanish falls back to English). Spanish strings follow
Windows 11 wording ("Anclado", "Todas las aplicaciones", "Recomendado",
"Mejor coincidencia", "Anclar a Inicio", "Inicio/apagado"...). The language
is re-read on `WM_SETTINGCHANGE`.

## Home

- **Search box**: native EDIT control. The placeholder ("Search for apps,
  settings, and documents" / "Buscar aplicaciones, configuración y
  documentos") is painted by subclassing the EDIT window, so it looks the
  same with or without the comctl32 v6 manifest (`EM_SETCUEBANNER` is not
  used).
- **Pinned**: 6×3 grid. More than 18 pins are paged: vertical page dots at
  the right of the grid (click), mouse wheel over Home, or PgUp/PgDn.
  Defaults are seeded once (`DEFAULT_PIN_IDS`).
- **Recommended**: up to 6 items, 2 columns: recently opened files (from
  `%APPDATA%\Microsoft\Windows\Recent`, newest `.lnk` first, targets
  resolved with `IShellLinkW` on a worker thread, missing targets and
  folders skipped, network targets not probed) with a relative time
  ("Today 14:32" / "Hoy 14:32", "Yesterday…"/"Ayer…", "12 min ago" /
  "Hace 12 min", "Oct 12" / "12 oct"), then frequently used apps. Recent
  files are hidden when Windows' "Show recently opened items in Start" is
  off (`HKCU\...\Explorer\Advanced\Start_TrackDocs = 0`). The list is
  refreshed on every show.
- **All apps** button opens the alphabetical list. A letter header opens a
  letter picker to jump.

## Search

Typing switches to the Search view. Results are sectioned like Windows:

| Section | Source |
|---|---|
| Best match | one large row, chosen by `results::assemble` |
| Apps | installed apps (fuzzy, prefix bonuses, frecency) and system tools |
| Settings | ~70 Settings pages (`ms-settings:` URIs) |
| Documents | file index daemon (`file-index`) or Everything (`everything`) |
| Commands | Run commands and the calculator |

Best match order: calculator result → app/tool whose name equals or starts
with the query → best app/tool/setting with a word-prefix match → document
whose name starts with the query → best fuzzy app/tool/setting → command →
document. Matching is accent-insensitive ("configuracion" finds
"Configuración").

**Settings table** (`explorer_core::settings_pages`): bilingual names and
keywords. The name in the UI language matches fuzzily; the other language's
name and the keywords match by word prefix, so "wifi", "brightness",
"fondo de pantalla" or "taskbar" (in Spanish) find the right page. Also
system tools: Control Panel, Windows Tools, Device Manager, Disk
Management, Services, Event Viewer, Computer Management, Task Scheduler,
Registry Editor, Task Manager, Resource Monitor, System Configuration,
System Information, Advanced system settings, environment variables,
Network Connections, Programs and Features, Command Prompt, PowerShell and
Terminal (only if `wt.exe` resolves). A tool with the same name as an
installed app is shown once.

**Calculator** (`explorer_core::calc`): `+ - * / ^ %`, `× ÷ −`,
parentheses, unary minus; `.` or `,` as the decimal separator (`3,5` and
`3.5`; `1.234,5` and `1,234.5`; ambiguous forms such as `1,5,3` are
rejected). `50%` is a percentage, `10 % 3` the remainder. A plain number is
not a calculation; a leading `=` forces one. Input length and nesting depth
are bounded; nothing panics. The result is shown as "= 42" (Spanish uses a
decimal comma). Enter copies it to the clipboard and shows "Copied" /
"Copiado"; Start stays open.

**Run commands** (`explorer_core::run_command`):
- `>command` runs in a Command Prompt (`cmd.exe /k`);
- paths (`C:\…`, `\\server\share`, `%VAR%\…`, `shell:…`) are opened;
- a bare program name (`notepad`, `regedit`, `devmgmt.msc args`) is offered
  only after it resolves through App Paths or `SearchPathW` (worker thread).
- Anything mentioning a web address (`http:`, `https:`, `www.`, any other
  URI scheme except `shell:` and `ms-settings:`) is never offered, not even
  as an argument.
- Ctrl+Shift+Enter runs as administrator.

**Detail panel** (right): icon, name, kind, path or expression, and the
actions of the focused result (`results::actions`): Open / Run / Copy, Run
as administrator, Open file location, Copy path, Pin to Start / Unpin from
Start, Pin to taskbar / Unpin from taskbar, Uninstall (opens
`ms-settings:appsfeatures`), Remove from list (Recommended items: deletes
the shortcut in the Recent folder). The same actions appear in every
right-click / context-menu-key menu; pinned items add "Move to front".

**Pin to taskbar** writes `AppPaths::taskbar_pins_file()` (seeded with
`DEFAULT_TASKBAR_PIN_IDS` first if the taskbar never wrote it); the taskbar
reloads the file when its modification time changes. The Start pins file
format is unchanged.

## Footer

Account picture (from `HKLM\...\AccountPicture\Users\<SID>\Image96`, made
round; falls back to a glyph) and user name — click: account settings,
Lock, Sign out. Right side: "…" (Start settings, Restart Start menu — exits
with `EXIT_RESTART` so the supervisor restarts it), File Explorer
(`explorer.exe`), Settings (`ms-settings:`), Power (Lock, Sign out, Sleep
if `GetPwrCapabilities` reports S1–S3 or Modern Standby, Hibernate only if
S4 and a hibernation file exist, Shut down, Restart). Footer buttons have
tooltips.

## Keyboard

Focus is logical (the caret stays in the search box). Zones per view:

- Home: pinned grid → Recommended → footer → "All apps" button;
- All apps: list → footer → "Back";
- Search: results → detail actions → footer.

Tab / Shift+Tab: search box → zones → search box (detail actions and footer
buttons one by one). Arrows move inside a zone and across neighbouring
zones (Left/Right only while the query is empty; otherwise they move the
caret). Enter runs the focused item, Ctrl+Shift+Enter elevated, the
context-menu key opens its menu, PgUp/PgDn page pins or scroll lists. Esc:
clear the query → back to Home (or from the letter picker to All apps) →
close. A focus ring is drawn after keyboard navigation; mouse hover
highlights without moving the keyboard focus.

## Performance

The UI thread only runs the in-memory app and settings search (well under a
millisecond for ~200 apps and ~85 table entries). Program resolution,
recent files, the account picture, optional-tool checks and icons run on
two worker threads (`start-aux`, `start-icons`); file results on the
`files.rs` worker. Stale answers are dropped by query generation.

## For the lead

Merged into ARCHITECTURE.md, STATUS.md and TESTING-ON-WINDOWS.md.
