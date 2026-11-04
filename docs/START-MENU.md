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

## For the lead: STATUS / TESTING / ARCHITECTURE entries

### STATUS rows (all unverified on Windows)

| State | Item |
|---|---|
| Implemented, unverified on Windows | Start menu bilingual UI (English/Spanish) and search placeholder (EDIT subclass) |
| Implemented, unverified on Windows | Sectioned search (Best match, Apps, Settings, Documents, Commands) with detail panel actions |
| Implemented, unverified on Windows | Settings pages and system tools search (`ms-settings:`, `.msc`, Terminal only if installed) |
| Implemented, unverified on Windows | Calculator in search, Enter copies (CF_UNICODETEXT) |
| Implemented, unverified on Windows | Run commands (`>cmd`, paths, programs via App Paths/SearchPathW), never URLs |
| Implemented, unverified on Windows | Recommended: recent files via IShellLinkW, honours Start_TrackDocs; frequently used apps |
| Implemented, unverified on Windows | Pinned pages (>18 pins), Pin to taskbar, Uninstall, Remove from list, Move to front |
| Implemented, unverified on Windows | Footer: account picture, File Explorer, Settings, "…" (restart Start menu), power menu with GetPwrCapabilities |
| Implemented, unverified on Windows | Keyboard focus zones, focus ring, Esc semantics, context-menu key |

### ARCHITECTURE

The launcher keeps decisions in `explorer-core` (`settings_pages`, `calc`,
`run_command`, `recent`, `results`) and in its portable `model.rs`; the
Windows layer (`app.rs`, `sys.rs`, `worker.rs`, `cue.rs`) only draws rows
and runs actions. Background work: indexer thread (apps + icons),
`start-aux` (program resolution, recent files, avatar, optional tools),
`start-icons` (on-demand icons), file-results worker.

### TESTING steps (Windows 11, Spanish UI)

1. Press Win: Start opens instantly above the taskbar with the placeholder
   "Buscar aplicaciones, configuración y documentos"; headers read
   "Anclado", "Todas las aplicaciones", "Recomendado".
2. Pin more than 18 apps (All apps → right-click → Anclar a Inicio): page
   dots appear right of the grid; wheel, dots and PgDn change pages.
3. Open a few documents in Notepad/Word, reopen Start: they appear under
   "Recomendado" with "Hoy HH:MM". Turn off Settings → Personalization →
   Start → "Show recently opened items": reopen Start, files disappear.
   Right-click a recent file → "Quitar de la lista".
4. Type `blo`: "Bloc de notas" is the Best match; the right panel shows
   Abrir, Abrir ubicación del archivo (classic apps), Anclar a Inicio,
   Anclar a la barra de tareas, Desinstalar. Choose "Anclar a la barra de
   tareas": the taskbar shows the pin within a few seconds.
5. Type `pantalla`, `wifi`, `bluetooth`, `fondo de pantalla`,
   `administrador de dispositivos`, `regedit`: the right Settings page /
   tool opens with Enter.
6. Type `6*7` → "= 42"; `1/4` → "= 0,25"; Enter shows "Copiado" and the
   clipboard holds `0,25`. Type `14:30`, `2024`: no calculator row.
7. Type `>ipconfig` → Enter opens a Command Prompt running ipconfig. Type
   `%APPDATA%`, `C:\Windows`, `notepad C:\Windows\win.ini`: each opens.
   Type `https://example.com`, `www.example.com`, `msedge https://x`: no
   Run row. Ctrl+Shift+Enter on `cmd` shows a UAC prompt.
8. Keyboard only: Tab from the search box through results, detail actions
   (one by one), footer buttons, back to the search box; arrows inside
   grids/lists; Esc clears the query, then returns Home, then closes. The
   focus ring is visible after keyboard moves.
9. Footer: account picture shown (or a person glyph); File Explorer and
   Settings buttons open; power menu hides "Suspender" / "Hibernar" when not
   supported; "…" → "Reiniciar el menú Inicio" restarts the launcher (under
   the supervisor).
10. With `--features file-index` and the daemon running, type part of a
    file name: "Documentos" shows files with icons and folders; actions
    Abrir / Abrir ubicación del archivo / Copiar ruta de acceso work.
11. Switch Windows to English (or `[general] language = "en"`): all strings
    are English.
