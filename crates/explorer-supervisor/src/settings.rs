// SPDX-License-Identifier: GPL-3.0-or-later
//! Portable model of the settings window (`explorer-rs --settings`).
//!
//! - [`SECTIONS`] describes every user-facing config key: where it lives,
//!   how it is edited (check box, drop-down, number, text, list) and its
//!   bilingual label and help text (taken from config/default.toml).
//! - [`inputs_from`] turns a [`Config`] into form values; [`apply`] turns
//!   the form values back into an edited TOML document.
//!
//! Saving must not destroy the user's file: [`apply`] edits the existing
//! text with `toml_edit`, touching only the keys whose value changed, so
//! comments, key order and unknown formatting survive. The result is
//! re-parsed as a [`Config`] (unknown keys and invalid values rejected)
//! before anything is written, and [`write_config`] replaces the file
//! atomically.
//!
//! The Win32 window lives in the binary (`settings_ui.rs`); everything here
//! runs on Linux and is unit tested.

use std::path::Path;

use explorer_core::config::{Config, DEFAULT_CONFIG_TOML};
use explorer_core::i18n::{tr, Lang};
use toml_edit::{Array, DocumentMut, Item, Table, TableLike, Value};

/// A text in both UI languages.
#[derive(Debug, Clone, Copy)]
pub struct Text {
    pub en: &'static str,
    pub es: &'static str,
}

impl Text {
    pub fn get(&self, lang: Lang) -> &'static str {
        tr(lang, self.en, self.es)
    }
}

const fn t(en: &'static str, es: &'static str) -> Text {
    Text { en, es }
}

/// One option of a drop-down (or one check box of a [`Kind::Flags`] set).
#[derive(Debug, Clone, Copy)]
pub struct Choice {
    /// The value written to the TOML file.
    pub value: &'static str,
    pub label: Text,
}

const fn c(value: &'static str, en: &'static str, es: &'static str) -> Choice {
    Choice {
        value,
        label: t(en, es),
    }
}

/// How a key is edited.
#[derive(Debug, Clone, Copy)]
pub enum Kind {
    /// Check box.
    Bool,
    /// Drop-down list; the value is a string.
    Choice(&'static [Choice]),
    /// Whole number in an edit field. The bounds mirror
    /// `Config::validate` where it has them and are otherwise generous
    /// sanity limits; they are only enforced for values the user changed.
    Int { min: i64, max: i64 },
    /// Single-line text.
    Text,
    /// List of strings edited one item per line.
    List,
    /// List of strings from a fixed set, one check box per option.
    Flags(&'static [Choice]),
}

/// One config key in the form.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    /// Dotted path in the TOML file, e.g. `launcher.everything.enabled`.
    pub path: &'static str,
    pub label: Text,
    pub help: Text,
    pub kind: Kind,
}

/// One page of the form.
#[derive(Debug, Clone, Copy)]
pub struct Section {
    pub title: Text,
    pub fields: &'static [Field],
}

/// Config keys deliberately left out of the window (still editable in the
/// file): `index.max_query_results` is a limit on the index daemon's IPC
/// protocol for programmatic clients, not something a user tunes.
pub const SKIPPED: &[&str] = &["index.max_query_results"];

const fn f(path: &'static str, label: Text, help: Text, kind: Kind) -> Field {
    Field {
        path,
        label,
        help,
        kind,
    }
}

const SYSTEM_DARK_LIGHT: &[Choice] = &[
    c("system", "Follow Windows", "Según Windows"),
    c("dark", "Dark", "Oscuro"),
    c("light", "Light", "Claro"),
];
const LANGUAGES: &[Choice] = &[
    c("system", "Follow Windows", "Según Windows"),
    c("en", "English", "English (inglés)"),
    c("es", "Español (Spanish)", "Español"),
];
const LOG_LEVELS: &[Choice] = &[
    c("error", "Errors only", "Solo errores"),
    c("warn", "Warnings", "Advertencias"),
    c("info", "Normal (info)", "Normal (info)"),
    c("debug", "Detailed (debug)", "Detallado (debug)"),
    c("trace", "Everything (trace)", "Todo (trace)"),
];
const EDGES: &[Choice] = &[c("bottom", "Bottom", "Abajo"), c("top", "Top", "Arriba")];
const ALIGNMENTS: &[Choice] = &[
    c("system", "Follow Windows", "Según Windows"),
    c("center", "Center", "Centro"),
    c("left", "Left", "Izquierda"),
];
const MONITORS: &[Choice] = &[
    c("all", "All monitors", "Todos los monitores"),
    c("primary", "Main monitor only", "Solo el monitor principal"),
];
const COMBINE: &[Choice] = &[
    c(
        "always",
        "Always (one button per app)",
        "Siempre (un botón por aplicación)",
    ),
    c(
        "never",
        "Never (one button per window)",
        "Nunca (un botón por ventana)",
    ),
];
const AUTO_HIDE: &[Choice] = &[
    c("system", "Follow Windows", "Según Windows"),
    c("always", "Always", "Siempre"),
    c("never", "Never", "Nunca"),
];
const FILE_SEARCH: &[Choice] = &[
    c(
        "auto",
        "As chosen in the installer",
        "Lo elegido en el instalador",
    ),
    c("on", "On", "Activada"),
    c("off", "Off", "Desactivada"),
];
const DESKTOP: &[Choice] = &[
    c(
        "shell",
        "Only when explorer.rs is the shell",
        "Solo cuando explorer.rs es el shell",
    ),
    c("always", "Always", "Siempre"),
    c("never", "Never", "Nunca"),
];
/// Order matters: with the first three ticked the list equals the default
/// `components`, which `components_explicit` does not count as explicit
/// (so `file_search = "auto"` keeps following the installer).
const COMPONENTS: &[Choice] = &[
    c("taskbar", "Taskbar", "Barra de tareas"),
    c("launcher", "Start menu", "Menú Inicio"),
    c("desktop", "Wallpaper", "Fondo de escritorio"),
    c("indexd", "File index", "Índice de archivos"),
];

/// Every page and key of the settings window, in display order.
pub const SECTIONS: &[Section] = &[
    Section {
        title: t("General", "General"),
        fields: &[
            f(
                "general.theme",
                t("Colours", "Colores"),
                t(
                    "Follow Windows' light/dark mode and accent colour, or force dark or light.",
                    "Seguir el modo claro/oscuro y el color de énfasis de Windows, o forzar oscuro o claro.",
                ),
                Kind::Choice(SYSTEM_DARK_LIGHT),
            ),
            f(
                "general.language",
                t("Language", "Idioma"),
                t(
                    "\"Follow Windows\" uses Spanish if Windows is in Spanish, English otherwise.",
                    "\"Según Windows\" usa español si Windows está en español, e inglés en otro caso.",
                ),
                Kind::Choice(LANGUAGES),
            ),
            f(
                "general.animations",
                t("Animations", "Animaciones"),
                t(
                    "Smooth opening slides, fades and hover transitions. Windows' \"Animation effects\" setting (Accessibility > Visual effects) also turns them off.",
                    "Deslizamientos, fundidos y transiciones suaves. El ajuste \"Efectos de animación\" de Windows (Accesibilidad > Efectos visuales) también los desactiva.",
                ),
                Kind::Bool,
            ),
            f(
                "general.log_level",
                t("Log detail", "Detalle del registro"),
                t(
                    "How much is written to the log files (for bug reports). The EXPLORER_RS_LOG environment variable overrides this.",
                    "Cuánto se escribe en los archivos de registro (para informar de errores). La variable de entorno EXPLORER_RS_LOG tiene prioridad.",
                ),
                Kind::Choice(LOG_LEVELS),
            ),
            f(
                "general.log_keep_files",
                t("Log files kept", "Archivos de registro guardados"),
                t(
                    "How many daily log files to keep per component.",
                    "Cuántos archivos de registro diarios se guardan por componente.",
                ),
                Kind::Int { min: 1, max: 365 },
            ),
        ],
    },
    Section {
        title: t("Taskbar", "Barra de tareas"),
        fields: &[
            f(
                "taskbar.position",
                t("Position", "Posición"),
                t("Screen edge of the taskbar.", "Borde de la pantalla de la barra de tareas."),
                Kind::Choice(EDGES),
            ),
            f(
                "taskbar.alignment",
                t("Alignment", "Alineación"),
                t(
                    "\"Follow Windows\" uses Settings > Personalization > Taskbar > Taskbar alignment.",
                    "\"Según Windows\" usa Configuración > Personalización > Barra de tareas > Alineación de la barra de tareas.",
                ),
                Kind::Choice(ALIGNMENTS),
            ),
            f(
                "taskbar.auto_hide",
                t("Automatically hide", "Ocultar automáticamente"),
                t(
                    "Hide the taskbar until the pointer touches the screen edge or Start opens. \"Follow Windows\" uses Windows' \"Automatically hide the taskbar\" setting.",
                    "Ocultar la barra hasta que el puntero toque el borde de la pantalla o se abra Inicio. \"Según Windows\" usa el ajuste \"Ocultar automáticamente la barra de tareas\" de Windows.",
                ),
                Kind::Choice(AUTO_HIDE),
            ),
            f(
                "taskbar.combine",
                t("Combine taskbar buttons", "Combinar botones de la barra de tareas"),
                t(
                    "Always: one button per app; several windows open a thumbnail picker. Never: one button per window.",
                    "Siempre: un botón por aplicación; varias ventanas abren un selector de miniaturas. Nunca: un botón por ventana.",
                ),
                Kind::Choice(COMBINE),
            ),
            f(
                "taskbar.button_labels",
                t("Show window titles", "Mostrar títulos de ventana"),
                t(
                    "Show window titles next to the icons (off = icons only, like Windows 11).",
                    "Mostrar los títulos junto a los iconos (desactivado = solo iconos, como Windows 11).",
                ),
                Kind::Bool,
            ),
            f(
                "taskbar.button_width",
                t("Labelled button width", "Ancho de los botones con título"),
                t(
                    "Width of a button with a title, in pixels at 100% scaling (40 to 1000).",
                    "Ancho de un botón con título, en píxeles al 100% de escala (40 a 1000).",
                ),
                Kind::Int { min: 40, max: 1000 },
            ),
            f(
                "taskbar.height",
                t("Height", "Altura"),
                t(
                    "Height in pixels at 100% scaling (16 to 200; Windows 11 uses 48).",
                    "Altura en píxeles al 100% de escala (16 a 200; Windows 11 usa 48).",
                ),
                Kind::Int { min: 16, max: 200 },
            ),
            f(
                "taskbar.monitors",
                t("Monitors", "Monitores"),
                t(
                    "Which monitors get a taskbar.",
                    "Qué monitores tienen barra de tareas.",
                ),
                Kind::Choice(MONITORS),
            ),
            f(
                "taskbar.windows_on_own_monitor_only",
                t(
                    "Only show windows on the taskbar's monitor",
                    "Mostrar solo las ventanas del monitor de cada barra",
                ),
                t(
                    "Each taskbar lists only the windows on its own monitor.",
                    "Cada barra muestra solo las ventanas de su propio monitor.",
                ),
                Kind::Bool,
            ),
            f(
                "taskbar.show_clock",
                t("Show the clock", "Mostrar el reloj"),
                t(
                    "Show the time and date at the end of the taskbar.",
                    "Mostrar la hora y la fecha al final de la barra de tareas.",
                ),
                Kind::Bool,
            ),
            f(
                "taskbar.clock_format",
                t("Time format", "Formato de la hora"),
                t(
                    "Supported codes: %H %M %S (24 h), %I %p (12 h), %d %m %Y %a %b.",
                    "Códigos admitidos: %H %M %S (24 h), %I %p (12 h), %d %m %Y %a %b.",
                ),
                Kind::Text,
            ),
            f(
                "taskbar.date_format",
                t("Date format", "Formato de la fecha"),
                t(
                    "Second line of the clock, same codes. Leave empty to hide it.",
                    "Segunda línea del reloj, mismos códigos. Déjalo vacío para ocultarla.",
                ),
                Kind::Text,
            ),
            f(
                "taskbar.hide_windows_taskbar",
                t("Hide Windows' taskbar", "Ocultar la barra de tareas de Windows"),
                t(
                    "When running alongside Windows Explorer, hide Windows' own taskbar. It comes back when explorer.rs stops, or with Ctrl+Alt+Shift+E.",
                    "Al funcionar junto al Explorador de Windows, ocultar la barra de tareas de Windows. Vuelve cuando explorer.rs se detiene, o con Ctrl+Alt+Mayús+E.",
                ),
                Kind::Bool,
            ),
            f(
                "taskbar.exclude_processes",
                t("Hidden programs", "Programas ocultos"),
                t(
                    "Program file names that never get a taskbar button, one per line (e.g. notepad.exe; upper/lower case does not matter).",
                    "Nombres de archivo de programas que nunca tienen botón en la barra, uno por línea (p. ej. notepad.exe; da igual mayúsculas o minúsculas).",
                ),
                Kind::List,
            ),
            f(
                "taskbar.exclude_classes",
                t("Hidden window classes", "Clases de ventana ocultas"),
                t(
                    "Window class names that never get a taskbar button, one per line (exact match).",
                    "Nombres de clase de ventana que nunca tienen botón en la barra, uno por línea (coincidencia exacta).",
                ),
                Kind::List,
            ),
        ],
    },
    Section {
        title: t("Notification area", "Área de notificación"),
        fields: &[
            f(
                "tray.enabled",
                t("Show the notification area", "Mostrar el área de notificación"),
                t(
                    "Show the system tray icons on the main monitor's taskbar.",
                    "Mostrar los iconos de la bandeja del sistema en la barra del monitor principal.",
                ),
                Kind::Bool,
            ),
            f(
                "tray.show_all",
                t("Show all icons", "Mostrar todos los iconos"),
                t(
                    "When off, only the icons turned on in Settings > Personalization > Taskbar > Other system tray icons are shown; the rest are behind the \"^\" button, like Windows 11.",
                    "Si está desactivado, solo se muestran los iconos activados en Configuración > Personalización > Barra de tareas > Otros iconos de la bandeja del sistema; el resto queda tras el botón \"^\", como en Windows 11.",
                ),
                Kind::Bool,
            ),
            f(
                "tray.intercept_in_dev_mode",
                t(
                    "Receive tray icons alongside Windows Explorer",
                    "Recibir los iconos junto al Explorador de Windows",
                ),
                t(
                    "Only when running alongside Windows Explorer: receive tray icons too (everything is passed on to Windows Explorer). Turn off if tray icons misbehave.",
                    "Solo al funcionar junto al Explorador de Windows: recibir también los iconos de la bandeja (todo se reenvía al Explorador). Desactívalo si los iconos fallan.",
                ),
                Kind::Bool,
            ),
        ],
    },
    Section {
        title: t("Start menu", "Menú Inicio"),
        fields: &[
            f(
                "launcher.win_key_opens_launcher",
                t("Windows key opens Start", "La tecla Windows abre Inicio"),
                t(
                    "Open the explorer.rs Start menu when the Windows key is pressed and released alone.",
                    "Abrir el menú Inicio de explorer.rs al pulsar y soltar sola la tecla Windows.",
                ),
                Kind::Bool,
            ),
            f(
                "launcher.width",
                t("Width", "Ancho"),
                t(
                    "Width of the Start menu in pixels at 100% scaling (at least 400).",
                    "Ancho del menú Inicio en píxeles al 100% de escala (al menos 400).",
                ),
                Kind::Int {
                    min: 400,
                    max: 10_000,
                },
            ),
            f(
                "launcher.height",
                t("Height", "Altura"),
                t(
                    "Height of the Start menu in pixels at 100% scaling (at least 400).",
                    "Altura del menú Inicio en píxeles al 100% de escala (al menos 400).",
                ),
                Kind::Int {
                    min: 400,
                    max: 10_000,
                },
            ),
            f(
                "launcher.max_results",
                t("Search results", "Resultados de búsqueda"),
                t(
                    "Maximum number of results shown (1 to 100).",
                    "Número máximo de resultados mostrados (1 a 100).",
                ),
                Kind::Int { min: 1, max: 100 },
            ),
            f(
                "launcher.extra_folders",
                t("Extra folders", "Carpetas adicionales"),
                t(
                    "Folders also searched for shortcuts and programs (.lnk, .url, .exe), one per line. The Start menu folders are always included.",
                    "Carpetas donde también se buscan accesos directos y programas (.lnk, .url, .exe), una por línea. Las carpetas del menú Inicio se incluyen siempre.",
                ),
                Kind::List,
            ),
            f(
                "launcher.everything.enabled",
                t("Search files with Everything", "Buscar archivos con Everything"),
                t(
                    "File search through Everything (voidtools). Needs a build with the \"everything\" feature and Everything running.",
                    "Búsqueda de archivos con Everything (voidtools). Requiere una compilación con la función \"everything\" y Everything en ejecución.",
                ),
                Kind::Bool,
            ),
            f(
                "launcher.everything.max_results",
                t("Everything results", "Resultados de Everything"),
                t(
                    "Number of Everything results shown.",
                    "Número de resultados de Everything mostrados.",
                ),
                Kind::Int { min: 0, max: 100 },
            ),
        ],
    },
    Section {
        title: t("Desktop", "Escritorio"),
        fields: &[f(
            "desktop.enabled",
            t("Wallpaper window", "Ventana del fondo de escritorio"),
            t(
                "When explorer.rs draws the wallpaper. Alongside Windows Explorer, Explorer already draws the desktop. There are no desktop icons.",
                "Cuándo dibuja explorer.rs el fondo de escritorio. Junto al Explorador de Windows, el Explorador ya dibuja el escritorio. No hay iconos en el escritorio.",
            ),
            Kind::Choice(DESKTOP),
        )],
    },
    Section {
        title: t("File search", "Búsqueda de archivos"),
        fields: &[
            f(
                "supervisor.file_search",
                t("Fast file search", "Búsqueda rápida de archivos"),
                t(
                    "Runs the file-name index so Start finds files instantly. \"As chosen in the installer\" applies unless the component list was changed.",
                    "Ejecuta el índice de nombres de archivo para que Inicio encuentre archivos al instante. \"Lo elegido en el instalador\" se aplica salvo que se haya cambiado la lista de componentes.",
                ),
                Kind::Choice(FILE_SEARCH),
            ),
            f(
                "index.launcher_results",
                t("File results in Start", "Resultados de archivos en Inicio"),
                t(
                    "Number of \"Files\" results in the Start menu (0 to 50).",
                    "Número de resultados de \"Archivos\" en el menú Inicio (0 a 50).",
                ),
                Kind::Int { min: 0, max: 50 },
            ),
            f(
                "index.roots",
                t("Folders to index", "Carpetas que indexar"),
                t(
                    "Folders or drives, one per line (e.g. C:\\ or D:\\Data). Empty means every local fixed drive.",
                    "Carpetas o unidades, una por línea (p. ej. C:\\ o D:\\Datos). Vacío significa todas las unidades locales fijas.",
                ),
                Kind::List,
            ),
            f(
                "index.use_ntfs",
                t("Read the NTFS file table", "Leer la tabla de archivos NTFS"),
                t(
                    "Fast start-up and live updates through the elevated helper. Without it, folders are walked and watched instead (slower start-up).",
                    "Inicio rápido y cambios al momento mediante el asistente con permisos elevados. Sin él, se recorren y vigilan las carpetas (inicio más lento).",
                ),
                Kind::Bool,
            ),
            f(
                "index.check_access",
                t(
                    "Hide files you cannot open",
                    "Ocultar archivos a los que no tienes acceso",
                ),
                t(
                    "Hide results inside folders your account cannot list. The NTFS table sees every file name on the drive.",
                    "Ocultar resultados dentro de carpetas que tu cuenta no puede ver. La tabla NTFS ve todos los nombres de archivo de la unidad.",
                ),
                Kind::Bool,
            ),
            f(
                "index.exclude_names",
                t("Excluded names", "Nombres excluidos"),
                t(
                    "File or folder names never indexed, one per line (upper/lower case does not matter; * wildcards allowed).",
                    "Nombres de archivo o carpeta que nunca se indexan, uno por línea (da igual mayúsculas o minúsculas; se admiten comodines *).",
                ),
                Kind::List,
            ),
            f(
                "index.exclude_paths",
                t("Excluded folders", "Carpetas excluidas"),
                t(
                    "Paths never indexed, one per line (e.g. C:\\Windows\\WinSxS).",
                    "Rutas que nunca se indexan, una por línea (p. ej. C:\\Windows\\WinSxS).",
                ),
                Kind::List,
            ),
            f(
                "index.snapshot_interval_s",
                t("Save the index every (seconds)", "Guardar el índice cada (segundos)"),
                t(
                    "0 = only when the index stops.",
                    "0 = solo cuando el índice se detiene.",
                ),
                Kind::Int {
                    min: 0,
                    max: 604_800,
                },
            ),
            f(
                "index.rescan_interval_s",
                t("Rescan every (seconds)", "Volver a recorrer cada (segundos)"),
                t(
                    "Full rescans of folders that cannot be watched for changes (0 = never).",
                    "Recorridos completos de las carpetas que no se pueden vigilar (0 = nunca).",
                ),
                Kind::Int {
                    min: 0,
                    max: 604_800,
                },
            ),
        ],
    },
    Section {
        title: t("Components", "Componentes"),
        fields: &[
            f(
                "supervisor.components",
                t("Run these parts", "Ejecutar estas partes"),
                t(
                    "Untick one to turn it off. Without the taskbar there is no taskbar at all, also when explorer.rs is your shell. The file index is easier to control with \"Fast file search\".",
                    "Desmarca una para desactivarla. Sin la barra de tareas no habrá ninguna barra, también cuando explorer.rs es tu shell. El índice de archivos se controla mejor con \"Búsqueda rápida de archivos\".",
                ),
                Kind::Flags(COMPONENTS),
            ),
            f(
                "supervisor.restart_initial_delay_ms",
                t("First restart delay (ms)", "Espera del primer reinicio (ms)"),
                t(
                    "Wait before restarting a part that crashed. Doubles on each crash up to the maximum below.",
                    "Espera antes de reiniciar una parte que ha fallado. Se duplica en cada fallo hasta el máximo de abajo.",
                ),
                Kind::Int {
                    min: 1,
                    max: 600_000,
                },
            ),
            f(
                "supervisor.restart_max_delay_ms",
                t("Maximum restart delay (ms)", "Espera máxima de reinicio (ms)"),
                t(
                    "Upper limit of the restart wait.",
                    "Límite superior de la espera de reinicio.",
                ),
                Kind::Int {
                    min: 1,
                    max: 3_600_000,
                },
            ),
            f(
                "supervisor.crash_loop_max",
                t("Crashes before giving up", "Fallos antes de rendirse"),
                t(
                    "A part that crashes more often than this within the time below is not restarted again until explorer.rs restarts.",
                    "Una parte que falla más veces que esto en el tiempo de abajo no se vuelve a reiniciar hasta que explorer.rs se reinicie.",
                ),
                Kind::Int { min: 1, max: 1000 },
            ),
            f(
                "supervisor.crash_loop_window_s",
                t("Crash counting window (seconds)", "Ventana de recuento de fallos (segundos)"),
                t(
                    "Time window for the crash count above.",
                    "Periodo de tiempo para el recuento de fallos de arriba.",
                ),
                Kind::Int {
                    min: 1,
                    max: 86_400,
                },
            ),
        ],
    },
];

/// Every field in display order (the index used by [`Input`] lists).
pub fn fields() -> impl Iterator<Item = &'static Field> {
    SECTIONS.iter().flat_map(|s| s.fields.iter())
}

/// Value of one form control, as read from (or written to) the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Bool(bool),
    /// Index into the field's choices.
    Choice(usize),
    /// Number, text, or list (one item per line).
    Text(String),
    /// One flag per choice of a [`Kind::Flags`] field.
    Flags(Vec<bool>),
}

/// A parsed, typed field value.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Parsed {
    Bool(bool),
    Str(String),
    Int(i64),
    List(Vec<String>),
}

/// The value at a dotted `path` of the serialized config.
fn lookup<'a>(root: &'a toml::Value, path: &str) -> Option<&'a toml::Value> {
    path.split('.').try_fold(root, |v, seg| v.get(seg))
}

fn config_value(cfg: &Config) -> toml::Value {
    // Every Config field serializes (strings, integers, booleans, arrays).
    toml::Value::try_from(cfg).unwrap_or_else(|_| toml::Value::Table(Default::default()))
}

fn current(field: &Field, root: &toml::Value) -> Parsed {
    let v = lookup(root, field.path);
    match field.kind {
        Kind::Bool => Parsed::Bool(v.and_then(toml::Value::as_bool).unwrap_or(false)),
        Kind::Int { .. } => Parsed::Int(v.and_then(toml::Value::as_integer).unwrap_or(0)),
        Kind::Choice(_) | Kind::Text => {
            Parsed::Str(v.and_then(toml::Value::as_str).unwrap_or("").to_string())
        }
        Kind::List | Kind::Flags(_) => Parsed::List(
            v.and_then(toml::Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        ),
    }
}

/// Form values showing `cfg`, one per [`fields`] entry.
pub fn inputs_from(cfg: &Config) -> Vec<Input> {
    let root = config_value(cfg);
    fields()
        .map(|field| match (field.kind, current(field, &root)) {
            (Kind::Bool, Parsed::Bool(b)) => Input::Bool(b),
            (Kind::Choice(opts), Parsed::Str(s)) => {
                Input::Choice(opts.iter().position(|o| o.value == s).unwrap_or(0))
            }
            (Kind::Int { .. }, Parsed::Int(n)) => Input::Text(n.to_string()),
            (Kind::Text, Parsed::Str(s)) => Input::Text(s),
            (Kind::List, Parsed::List(l)) => Input::Text(l.join("\n")),
            (Kind::Flags(opts), Parsed::List(l)) => Input::Flags(
                opts.iter()
                    .map(|o| l.iter().any(|x| x == o.value))
                    .collect(),
            ),
            // `current` always returns the variant matching the kind.
            _ => Input::Text(String::new()),
        })
        .collect()
}

/// Split a multi-line edit into list items: one per line, trimmed, blank
/// lines dropped. Accepts `\n` and `\r\n`.
pub fn parse_lines(s: &str) -> Vec<String> {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Why the form could not be saved. `field` is the index of the offending
/// field (to focus it), when known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveError {
    pub field: Option<usize>,
    pub en: String,
    pub es: String,
}

impl SaveError {
    pub fn message(&self, lang: Lang) -> &str {
        match lang {
            Lang::En => &self.en,
            Lang::Es => &self.es,
        }
    }
}

fn parse_input(field: &Field, input: &Input) -> Result<Parsed, (String, String)> {
    let mismatch = || {
        (
            "internal error: wrong control type".to_string(),
            "error interno: tipo de control incorrecto".to_string(),
        )
    };
    match (field.kind, input) {
        (Kind::Bool, Input::Bool(b)) => Ok(Parsed::Bool(*b)),
        (Kind::Choice(opts), Input::Choice(i)) => opts
            .get(*i)
            .map(|o| Parsed::Str(o.value.to_string()))
            .ok_or_else(mismatch),
        (Kind::Int { min, max }, Input::Text(s)) => s
            .trim()
            .parse::<i64>()
            .map(Parsed::Int)
            .map_err(|_| range_error(min, max)),
        // Not trimmed: a format string may end in a deliberate space.
        (Kind::Text, Input::Text(s)) => Ok(Parsed::Str(s.clone())),
        (Kind::List, Input::Text(s)) => Ok(Parsed::List(parse_lines(s))),
        (Kind::Flags(opts), Input::Flags(on)) if on.len() == opts.len() => Ok(Parsed::List(
            opts.iter()
                .zip(on)
                .filter(|(_, on)| **on)
                .map(|(o, _)| o.value.to_string())
                .collect(),
        )),
        _ => Err(mismatch()),
    }
}

fn range_error(min: i64, max: i64) -> (String, String) {
    (
        format!("Enter a whole number from {min} to {max}."),
        format!("Escribe un número entero de {min} a {max}."),
    )
}

/// Same value as far as the config is concerned? Flags are a set: their
/// order in the file is kept unless the set changes.
fn same(field: &Field, a: &Parsed, b: &Parsed) -> bool {
    match (field.kind, a, b) {
        (Kind::Flags(_), Parsed::List(x), Parsed::List(y)) => {
            let (mut x, mut y) = (x.clone(), y.clone());
            x.sort();
            x.dedup();
            y.sort();
            y.dedup();
            x == y
        }
        _ => a == b,
    }
}

/// Lists longer than this (rendered) are written one item per line.
const ONE_LINE_ARRAY_MAX: usize = 60;

fn to_toml(p: &Parsed) -> Value {
    match p {
        Parsed::Bool(b) => Value::from(*b),
        Parsed::Int(n) => Value::from(*n),
        Parsed::Str(s) => Value::from(s.as_str()),
        Parsed::List(items) => {
            let mut arr: Array = items.iter().map(String::as_str).collect();
            if arr.to_string().len() > ONE_LINE_ARRAY_MAX {
                for v in arr.iter_mut() {
                    v.decor_mut().set_prefix("\n    ");
                }
                arr.set_trailing_comma(true);
                arr.set_trailing("\n");
            }
            Value::Array(arr)
        }
    }
}

/// Set `path` (dotted) in `doc` to `value`, creating missing tables. An
/// existing value keeps its decoration (spacing, trailing comment).
fn set_path(doc: &mut DocumentMut, path: &str, mut value: Value) -> Result<(), String> {
    let segs: Vec<&str> = path.split('.').collect();
    let Some((last, parents)) = segs.split_last() else {
        return Err("empty key".into());
    };
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    for (i, seg) in parents.iter().enumerate() {
        if table.get(seg).is_none() {
            let mut t = Table::new();
            // `[launcher.everything]` alone must not add an empty
            // `[launcher]` header.
            t.set_implicit(i + 1 < parents.len());
            table.insert(seg, Item::Table(t));
        }
        table = table
            .get_mut(seg)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| format!("{seg} is not a table"))?;
    }
    match table.get_mut(last) {
        Some(Item::Value(old)) => {
            *value.decor_mut() = old.decor().clone();
            *old = value;
        }
        Some(Item::None) | None => {
            table.insert(last, Item::Value(value));
        }
        Some(_) => return Err(format!("{path} is not a value")),
    }
    Ok(())
}

/// Result of [`apply`].
#[derive(Debug, Clone)]
pub struct Applied {
    /// The new file contents (equal to the input when nothing changed).
    pub text: String,
    /// The config the new text parses to.
    pub config: Config,
    /// Indices of the changed fields.
    pub changed: Vec<usize>,
}

/// Apply the form to `base_text` (the config file's current contents, or
/// [`DEFAULT_CONFIG_TOML`] when there is no file yet).
///
/// A field counts as changed when its value differs from `shown`, the
/// config the form was filled from; only changed keys are written, so the
/// rest of `base_text` is untouched (including keys missing from it, which
/// keep following the built-in defaults). Bounds are only checked for
/// changed fields, so an odd hand-written value elsewhere never blocks
/// saving. The result must parse as a valid [`Config`].
pub fn apply(base_text: &str, shown: &Config, inputs: &[Input]) -> Result<Applied, SaveError> {
    let root = config_value(shown);
    let mut doc: DocumentMut = base_text
        .parse()
        .map_err(|e: toml_edit::TomlError| SaveError {
            field: None,
            en: format!("The configuration file is not valid TOML:\n{e}"),
            es: format!("El archivo de configuración no es TOML válido:\n{e}"),
        })?;
    let mut changed = Vec::new();
    for (i, (field, input)) in fields().zip(inputs).enumerate() {
        let new = parse_input(field, input).map_err(|(en, es)| SaveError {
            field: Some(i),
            en,
            es,
        })?;
        if same(field, &new, &current(field, &root)) {
            continue;
        }
        if let (Kind::Int { min, max }, Parsed::Int(n)) = (field.kind, &new) {
            if !(min..=max).contains(n) {
                let (en, es) = range_error(min, max);
                return Err(SaveError {
                    field: Some(i),
                    en,
                    es,
                });
            }
        }
        set_path(&mut doc, field.path, to_toml(&new)).map_err(|e| SaveError {
            field: Some(i),
            en: format!("Cannot change {}: {e}", field.path),
            es: format!("No se puede cambiar {}: {e}", field.path),
        })?;
        changed.push(i);
    }
    let text = if changed.is_empty() {
        base_text.to_string()
    } else {
        doc.to_string()
    };
    let config = Config::from_toml(&text).map_err(|e| {
        let msg = e.to_string();
        SaveError {
            // Validation messages name the key ("taskbar.height must ...").
            field: fields().position(|f| msg.contains(f.path)),
            en: format!("These settings are not valid:\n{msg}"),
            es: format!("Estos ajustes no son válidos:\n{msg}"),
        }
    })?;
    Ok(Applied {
        text,
        config,
        changed,
    })
}

/// The config file as found on disk.
#[derive(Debug, Clone)]
pub struct Loaded {
    /// File contents; `None` if the file does not exist yet.
    pub text: Option<String>,
    pub config: Config,
}

impl Loaded {
    /// Text to edit: the file, or the documented default for a new file
    /// (the same content `--edit-config` creates).
    pub fn base_text(&self) -> &str {
        self.text.as_deref().unwrap_or(DEFAULT_CONFIG_TOML)
    }
}

/// Read and parse the config file. A missing file is the default config;
/// an unreadable or invalid one is an error (the window then offers the
/// text editor instead of overwriting it).
pub fn load(path: &Path) -> Result<Loaded, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let config = Config::from_toml(&text).map_err(|e| e.to_string())?;
            Ok(Loaded {
                text: Some(text),
                config,
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Loaded {
            text: None,
            config: Config::default(),
        }),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Write the config file atomically (temporary file, then rename), so the
/// supervisor's config watcher never sees a half-written file.
pub fn write_config(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    explorer_core::fsutil::write_atomic(path, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use explorer_core::config::{Edge, FileSearch};

    fn index_of(path: &str) -> usize {
        fields().position(|f| f.path == path).unwrap()
    }

    fn set(inputs: &mut [Input], path: &str, v: Input) {
        inputs[index_of(path)] = v;
    }

    /// Every user-facing key of the config is in the form or in SKIPPED,
    /// and every form path exists.
    #[test]
    fn form_covers_every_key() {
        let root = config_value(&Config::default());
        let mut keys = Vec::new();
        fn walk(prefix: &str, v: &toml::Value, out: &mut Vec<String>) {
            match v.as_table() {
                Some(t) => {
                    for (k, v) in t {
                        let p = if prefix.is_empty() {
                            k.clone()
                        } else {
                            format!("{prefix}.{k}")
                        };
                        walk(&p, v, out);
                    }
                }
                None => out.push(prefix.to_string()),
            }
        }
        walk("", &root, &mut keys);
        let form: Vec<&str> = fields().map(|f| f.path).collect();
        for k in &keys {
            assert!(
                form.contains(&k.as_str()) || SKIPPED.contains(&k.as_str()),
                "{k} is neither in the form nor in SKIPPED"
            );
        }
        for p in form.iter().chain(SKIPPED) {
            assert!(lookup(&root, p).is_some(), "{p} is not a config key");
        }
        let mut sorted = form.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), form.len(), "duplicate field");
    }

    /// Choice values are exactly what the config accepts.
    #[test]
    fn every_choice_parses() {
        for field in fields() {
            if let Kind::Choice(opts) | Kind::Flags(opts) = field.kind {
                for o in opts {
                    let toml = match field.kind {
                        Kind::Flags(_) => format!("[x]\nk = [\"{}\"]", o.value),
                        _ => format!("[x]\nk = \"{}\"", o.value),
                    };
                    let (section, key) = field.path.rsplit_once('.').unwrap();
                    let toml = toml
                        .replace("[x]", &format!("[{section}]"))
                        .replace("k =", &format!("{key} ="));
                    assert!(
                        Config::from_toml(&toml).is_ok(),
                        "{} = {} rejected",
                        field.path,
                        o.value
                    );
                }
            }
        }
    }

    #[test]
    fn unchanged_form_changes_nothing() {
        let text = "# my file\n[taskbar]\nheight = 40 # tall\n";
        let cfg = Config::from_toml(text).unwrap();
        let a = apply(text, &cfg, &inputs_from(&cfg)).unwrap();
        assert!(a.changed.is_empty());
        assert_eq!(a.text, text);
    }

    #[test]
    fn only_changed_keys_are_written_and_comments_survive() {
        let text = "# my comment\n[taskbar]\n# about height\nheight = 40 # tall\nshow_clock = true\n\n[general]\ntheme = \"dark\"\n";
        let cfg = Config::from_toml(text).unwrap();
        let mut inputs = inputs_from(&cfg);
        set(&mut inputs, "taskbar.height", Input::Text(" 56 ".into()));
        set(&mut inputs, "taskbar.position", Input::Choice(1));
        let a = apply(text, &cfg, &inputs).unwrap();
        assert_eq!(a.changed.len(), 2);
        assert_eq!(a.config.taskbar.height, 56);
        assert_eq!(a.config.taskbar.position, Edge::Top);
        assert!(a.text.contains("# my comment\n"));
        assert!(a.text.contains("# about height\nheight = 56 # tall\n"));
        assert!(a.text.contains("position = \"top\""));
        assert!(a.text.contains("theme = \"dark\""));
        // Untouched defaults are not spelled out.
        assert!(!a.text.contains("button_width"));
        // The new key went into the existing [taskbar] table.
        assert_eq!(a.text.matches("[taskbar]").count(), 1);
        let pos = a.text.find("position").unwrap();
        assert!(pos < a.text.find("[general]").unwrap());
    }

    #[test]
    fn missing_tables_are_created() {
        let text = "# empty\n";
        let cfg = Config::from_toml(text).unwrap();
        let mut inputs = inputs_from(&cfg);
        set(
            &mut inputs,
            "launcher.everything.enabled",
            Input::Bool(true),
        );
        set(&mut inputs, "supervisor.file_search", Input::Choice(2));
        let a = apply(text, &cfg, &inputs).unwrap();
        assert!(a.config.launcher.everything.enabled);
        assert_eq!(a.config.supervisor.file_search, FileSearch::Off);
        assert!(a.text.contains("[launcher.everything]"));
        // Implicit parent: no empty [launcher] header.
        assert!(!a.text.contains("[launcher]\n"));
    }

    #[test]
    fn inline_and_dotted_tables_are_edited_in_place() {
        let text = "taskbar = { height = 40 }\ngeneral.theme = \"dark\"\n";
        let cfg = Config::from_toml(text).unwrap();
        let mut inputs = inputs_from(&cfg);
        set(&mut inputs, "taskbar.height", Input::Text("60".into()));
        set(&mut inputs, "general.animations", Input::Bool(false));
        let a = apply(text, &cfg, &inputs).unwrap();
        assert_eq!(a.config.taskbar.height, 60);
        assert!(!a.config.general.animations);
        assert!(a.text.starts_with("taskbar = { height = 60 }"));
        assert_eq!(
            a.config.general.theme,
            explorer_core::config::ThemeMode::Dark
        );
    }

    #[test]
    fn new_file_starts_from_documented_default() {
        let loaded = Loaded {
            text: None,
            config: Config::default(),
        };
        let mut inputs = inputs_from(&loaded.config);
        set(&mut inputs, "taskbar.show_clock", Input::Bool(false));
        let a = apply(loaded.base_text(), &loaded.config, &inputs).unwrap();
        assert!(!a.config.taskbar.show_clock);
        // The documentation comments of the default file are kept.
        assert!(a.text.contains("# explorer.rs default configuration."));
        assert!(a.text.contains("# Show the clock.\nshow_clock = false\n"));
        assert_eq!(a.changed, vec![index_of("taskbar.show_clock")]);
    }

    #[test]
    fn numbers_are_validated() {
        let cfg = Config::default();
        let mut inputs = inputs_from(&cfg);
        set(&mut inputs, "taskbar.height", Input::Text("tall".into()));
        let e = apply("", &cfg, &inputs).unwrap_err();
        assert_eq!(e.field, Some(index_of("taskbar.height")));
        assert!(e.message(Lang::Es).contains("16 a 200"));
        set(&mut inputs, "taskbar.height", Input::Text("500".into()));
        let e = apply("", &cfg, &inputs).unwrap_err();
        assert_eq!(e.field, Some(index_of("taskbar.height")));
        assert!(e.message(Lang::En).contains("16 to 200"));
    }

    #[test]
    fn out_of_range_values_already_in_the_file_do_not_block_saving() {
        // log_keep_files = 0 is accepted by Config but below the form's
        // bound; it is not touched, so saving another key works.
        let text = "[general]\nlog_keep_files = 0\n";
        let cfg = Config::from_toml(text).unwrap();
        let mut inputs = inputs_from(&cfg);
        set(&mut inputs, "general.animations", Input::Bool(false));
        let a = apply(text, &cfg, &inputs).unwrap();
        assert!(a.text.contains("log_keep_files = 0"));
    }

    #[test]
    fn cross_field_validation_points_at_a_field() {
        let cfg = Config::default();
        let mut inputs = inputs_from(&cfg);
        set(
            &mut inputs,
            "supervisor.restart_initial_delay_ms",
            Input::Text("40000".into()),
        );
        let e = apply("", &cfg, &inputs).unwrap_err();
        assert_eq!(
            e.field,
            Some(index_of("supervisor.restart_initial_delay_ms"))
        );
        assert!(e.en.contains("restart_max_delay_ms"));
    }

    #[test]
    fn lists_one_item_per_line() {
        assert_eq!(
            parse_lines("  a.exe \r\n\r\nb.exe\n  \n"),
            vec!["a.exe".to_string(), "b.exe".to_string()]
        );
        let cfg = Config::default();
        let mut inputs = inputs_from(&cfg);
        set(
            &mut inputs,
            "taskbar.exclude_processes",
            Input::Text("Foo.exe\r\nbar.exe\r\n".into()),
        );
        set(
            &mut inputs,
            "index.roots",
            Input::Text("C:\\\nD:\\Data with a long name\\and more\\and even more and more".into()),
        );
        let a = apply(DEFAULT_CONFIG_TOML, &cfg, &inputs).unwrap();
        assert_eq!(a.config.taskbar.exclude_processes, ["Foo.exe", "bar.exe"]);
        assert_eq!(
            a.config.index.roots,
            [
                "C:\\",
                "D:\\Data with a long name\\and more\\and even more and more"
            ]
        );
        assert!(a
            .text
            .contains("exclude_processes = [\"Foo.exe\", \"bar.exe\"]"));
        // Long lists are written one item per line.
        assert!(a.text.contains("roots = [\n    "), "{}", a.text);
        // And read back into the form unchanged.
        let back = inputs_from(&a.config);
        assert_eq!(
            back[index_of("taskbar.exclude_processes")],
            Input::Text("Foo.exe\nbar.exe".into())
        );
    }

    #[test]
    fn components_are_a_set() {
        let text = "[supervisor]\ncomponents = [\"launcher\", \"taskbar\"]\n";
        let cfg = Config::from_toml(text).unwrap();
        let inputs = inputs_from(&cfg);
        assert_eq!(
            inputs[index_of("supervisor.components")],
            Input::Flags(vec![true, true, false, false])
        );
        // Same set in another order: not a change.
        assert!(apply(text, &cfg, &inputs).unwrap().changed.is_empty());

        // The default set is written in the default order, so the list does
        // not count as explicit (file_search = "auto" keeps working).
        let mut inputs = inputs;
        set(
            &mut inputs,
            "supervisor.components",
            Input::Flags(vec![true, true, true, false]),
        );
        let a = apply(text, &cfg, &inputs).unwrap();
        assert_eq!(
            a.config.supervisor.components,
            explorer_core::config::SupervisorConfig::default().components
        );
        assert!(!explorer_core::components::components_explicit(&a.text));
    }

    #[test]
    fn invalid_base_text_is_reported() {
        let cfg = Config::default();
        let e = apply("this is = = not toml", &cfg, &inputs_from(&cfg)).unwrap_err();
        assert_eq!(e.field, None);
        // Unknown keys in the file are caught by the re-parse.
        let mut inputs = inputs_from(&cfg);
        set(&mut inputs, "taskbar.height", Input::Text("40".into()));
        assert!(apply("[taskbar]\nheigth = 3\n", &cfg, &inputs).is_err());
    }

    #[test]
    fn load_and_write() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("config.toml");
        let l = load(&p).unwrap();
        assert!(l.text.is_none());
        assert_eq!(l.base_text(), DEFAULT_CONFIG_TOML);
        write_config(&p, "[taskbar]\nheight = 33\n").unwrap();
        let l = load(&p).unwrap();
        assert_eq!(l.config.taskbar.height, 33);
        // No temporary files are left behind.
        assert_eq!(std::fs::read_dir(p.parent().unwrap()).unwrap().count(), 1);
        std::fs::write(&p, "[taskbar]\nheigth = 3\n").unwrap();
        assert!(load(&p).is_err());
    }

    #[test]
    fn every_text_is_translated() {
        for s in SECTIONS {
            assert!(!s.title.en.is_empty() && !s.title.es.is_empty());
            for f in s.fields {
                for t in [f.label, f.help] {
                    assert!(!t.en.is_empty() && !t.es.is_empty(), "{}", f.path);
                }
            }
        }
    }
}
