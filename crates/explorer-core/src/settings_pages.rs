// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows Settings pages and system tools found by the Start menu search.
//!
//! A static bilingual table: each entry has an English and a Spanish name
//! (Windows 11 wording) plus search keywords in both languages, so a Spanish
//! user typing "wifi", "bluetooth" or "fondo de pantalla" and an English user
//! typing "brightness" both land on the right page. Matching uses the same
//! fuzzy scorer as apps ([`crate::search::Searcher::score`]): the name in the
//! UI language scores fully, the other language's name and all keywords count
//! as keywords.
//!
//! Settings pages are opened with `ShellExecute(ms-settings:...)`; tools are
//! programs or snap-ins started the same way (`devmgmt.msc`, `regedit.exe`).

use crate::i18n::Lang;
use crate::search::{Query, Searcher};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A page of the Settings app (`ms-settings:` URI or similar).
    Settings,
    /// A classic tool (Control Panel, MMC snap-in, console...).
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsEntry {
    /// Stable identifier (used as a key for icons, pins and tests).
    pub id: &'static str,
    /// What `ShellExecute` opens: a URI or a program.
    pub target: &'static str,
    /// Arguments for `target` (usually empty).
    pub args: &'static str,
    pub name_en: &'static str,
    pub name_es: &'static str,
    pub keywords_en: &'static [&'static str],
    pub keywords_es: &'static [&'static str],
    pub kind: EntryKind,
    /// Program that must exist for the entry to be shown (e.g. `wt.exe`).
    pub requires: Option<&'static str>,
}

impl SettingsEntry {
    pub fn name(&self, lang: Lang) -> &'static str {
        match lang {
            Lang::En => self.name_en,
            Lang::Es => self.name_es,
        }
    }

    /// All searchable words except the name in `lang`.
    pub fn keywords(&self, lang: Lang) -> impl Iterator<Item = &'static str> + '_ {
        let other = match lang {
            Lang::En => self.name_es,
            Lang::Es => self.name_en,
        };
        std::iter::once(other)
            .chain(self.keywords_en.iter().copied())
            .chain(self.keywords_es.iter().copied())
    }
}

macro_rules! entry {
    ($kind:ident, $id:literal, $target:literal, $args:literal, $en:literal, $es:literal,
     [$($ken:literal),* $(,)?], [$($kes:literal),* $(,)?] $(, requires = $req:literal)?) => {
        SettingsEntry {
            id: $id,
            target: $target,
            args: $args,
            name_en: $en,
            name_es: $es,
            keywords_en: &[$($ken),*],
            keywords_es: &[$($kes),*],
            kind: EntryKind::$kind,
            requires: entry!(@req $($req)?),
        }
    };
    (@req) => { None };
    (@req $r:literal) => { Some($r) };
}

macro_rules! page {
    ($id:literal, $uri:literal, $en:literal, $es:literal, [$($ken:literal),* $(,)?], [$($kes:literal),* $(,)?]) => {
        entry!(Settings, $id, $uri, "", $en, $es, [$($ken),*], [$($kes),*])
    };
}

/// The table. Order is irrelevant for search; keep related pages together.
pub const ENTRIES: &[SettingsEntry] = &[
    page!(
        "settings",
        "ms-settings:",
        "Settings",
        "Configuración",
        ["preferences", "options", "control"],
        ["ajustes", "opciones", "preferencias"]
    ),
    // System
    page!(
        "display",
        "ms-settings:display",
        "Display",
        "Pantalla",
        [
            "screen",
            "resolution",
            "brightness",
            "scale",
            "monitor",
            "orientation",
            "hdr",
            "refresh rate"
        ],
        [
            "resolución",
            "brillo",
            "escala",
            "monitor",
            "orientación",
            "frecuencia de actualización"
        ]
    ),
    page!(
        "nightlight",
        "ms-settings:nightlight",
        "Night light",
        "Luz nocturna",
        ["blue light", "warm", "night mode"],
        ["luz azul", "modo nocturno", "cálida"]
    ),
    page!(
        "graphics",
        "ms-settings:display-advancedgraphics",
        "Graphics",
        "Gráficos",
        ["gpu", "graphics card", "performance"],
        ["tarjeta gráfica", "gpu", "rendimiento"]
    ),
    page!(
        "sound",
        "ms-settings:sound",
        "Sound",
        "Sonido",
        [
            "audio",
            "volume",
            "speakers",
            "headphones",
            "microphone",
            "output",
            "input"
        ],
        [
            "audio",
            "volumen",
            "altavoces",
            "auriculares",
            "micrófono",
            "salida",
            "entrada"
        ]
    ),
    page!(
        "volumemixer",
        "ms-settings:apps-volume",
        "Volume mixer",
        "Mezclador de volumen",
        ["app volume", "audio"],
        ["volumen de aplicaciones", "audio"]
    ),
    page!(
        "notifications",
        "ms-settings:notifications",
        "Notifications",
        "Notificaciones",
        ["alerts", "toast", "do not disturb"],
        ["alertas", "avisos", "no molestar"]
    ),
    page!(
        "focus",
        "ms-settings:focus",
        "Focus",
        "Concentración",
        ["focus assist", "do not disturb", "quiet hours"],
        ["asistente de concentración", "no molestar"]
    ),
    page!(
        "power",
        "ms-settings:powersleep",
        "Power & battery",
        "Energía y batería",
        [
            "sleep",
            "screen timeout",
            "power mode",
            "battery",
            "shutdown",
            "energy"
        ],
        [
            "suspensión",
            "apagar pantalla",
            "modo de energía",
            "batería",
            "ahorro"
        ]
    ),
    page!(
        "batterysaver",
        "ms-settings:batterysaver",
        "Battery saver",
        "Ahorro de batería",
        ["energy saver", "battery"],
        ["ahorro de energía", "batería"]
    ),
    page!(
        "storage",
        "ms-settings:storagesense",
        "Storage",
        "Almacenamiento",
        [
            "disk space",
            "storage sense",
            "cleanup",
            "temporary files",
            "drives"
        ],
        [
            "espacio en disco",
            "sensor de almacenamiento",
            "liberar espacio",
            "archivos temporales",
            "unidades"
        ]
    ),
    page!(
        "multitasking",
        "ms-settings:multitasking",
        "Multitasking",
        "Multitarea",
        ["snap", "snap windows", "alt tab", "virtual desktops"],
        ["acoplar", "acoplar ventanas", "escritorios virtuales"]
    ),
    page!(
        "activation",
        "ms-settings:activation",
        "Activation",
        "Activación",
        ["product key", "license", "edition"],
        ["clave de producto", "licencia", "edición"]
    ),
    page!(
        "troubleshoot",
        "ms-settings:troubleshoot",
        "Troubleshoot",
        "Solucionar problemas",
        ["fix", "problems", "diagnostics", "repair"],
        ["arreglar", "problemas", "diagnóstico", "reparar"]
    ),
    page!(
        "recovery",
        "ms-settings:recovery",
        "Recovery",
        "Recuperación",
        [
            "reset this pc",
            "reset",
            "advanced startup",
            "go back",
            "restore"
        ],
        [
            "restablecer este equipo",
            "restablecer",
            "inicio avanzado",
            "volver",
            "restaurar"
        ]
    ),
    page!(
        "remotedesktop",
        "ms-settings:remotedesktop",
        "Remote Desktop",
        "Escritorio remoto",
        ["rdp", "remote access"],
        ["rdp", "acceso remoto"]
    ),
    page!(
        "clipboard",
        "ms-settings:clipboard",
        "Clipboard",
        "Portapapeles",
        ["clipboard history", "copy paste"],
        ["historial del portapapeles", "copiar pegar"]
    ),
    page!(
        "about",
        "ms-settings:about",
        "About",
        "Información",
        [
            "system info",
            "pc name",
            "rename this pc",
            "specifications",
            "windows version",
            "ram",
            "processor"
        ],
        [
            "información del sistema",
            "nombre del equipo",
            "cambiar nombre",
            "especificaciones",
            "versión de windows",
            "memoria",
            "procesador"
        ]
    ),
    page!(
        "optionalfeatures",
        "ms-settings:optionalfeatures",
        "Optional features",
        "Características opcionales",
        ["features", "add feature"],
        ["características", "agregar característica"]
    ),
    // Bluetooth & devices
    page!(
        "bluetooth",
        "ms-settings:bluetooth",
        "Bluetooth & devices",
        "Bluetooth y dispositivos",
        ["bluetooth", "devices", "pair", "add device"],
        ["dispositivos", "emparejar", "agregar dispositivo"]
    ),
    page!(
        "printers",
        "ms-settings:printers",
        "Printers & scanners",
        "Impresoras y escáneres",
        ["printer", "scanner", "print"],
        ["impresora", "escáner", "imprimir"]
    ),
    page!(
        "mouse",
        "ms-settings:mousetouchpad",
        "Mouse",
        "Mouse",
        ["pointer", "cursor", "scroll", "buttons"],
        ["ratón", "puntero", "cursor", "desplazamiento", "botones"]
    ),
    page!(
        "touchpad",
        "ms-settings:devices-touchpad",
        "Touchpad",
        "Panel táctil",
        ["trackpad", "gestures"],
        ["touchpad", "gestos"]
    ),
    page!(
        "typing",
        "ms-settings:typing",
        "Typing",
        "Escritura",
        ["autocorrect", "spelling", "touch keyboard"],
        ["autocorrección", "ortografía", "teclado táctil"]
    ),
    page!(
        "autoplay",
        "ms-settings:autoplay",
        "AutoPlay",
        "Reproducción automática",
        ["autorun", "removable drives"],
        ["unidades extraíbles"]
    ),
    page!(
        "usb",
        "ms-settings:usb",
        "USB",
        "USB",
        ["usb devices"],
        ["dispositivos usb"]
    ),
    page!(
        "camera",
        "ms-settings:camera",
        "Cameras",
        "Cámaras",
        ["webcam", "camera"],
        ["cámara web", "cámara"]
    ),
    // Network & internet
    page!(
        "network",
        "ms-settings:network",
        "Network & internet",
        "Red e Internet",
        ["network", "internet", "connection", "ethernet", "wifi"],
        ["red", "conexión", "internet", "ethernet", "wifi"]
    ),
    page!(
        "wifi",
        "ms-settings:network-wifi",
        "Wi-Fi",
        "Wi-Fi",
        ["wifi", "wireless", "wlan", "hotspot networks"],
        ["wifi", "inalámbrica", "redes inalámbricas"]
    ),
    page!(
        "ethernet",
        "ms-settings:network-ethernet",
        "Ethernet",
        "Ethernet",
        ["cable", "lan", "wired"],
        ["cable", "red cableada"]
    ),
    page!(
        "vpn",
        "ms-settings:network-vpn",
        "VPN",
        "VPN",
        ["virtual private network"],
        ["red privada virtual"]
    ),
    page!(
        "proxy",
        "ms-settings:network-proxy",
        "Proxy",
        "Proxy",
        ["proxy server", "pac"],
        ["servidor proxy"]
    ),
    page!(
        "hotspot",
        "ms-settings:network-mobilehotspot",
        "Mobile hotspot",
        "Zona con cobertura inalámbrica móvil",
        ["tethering", "share internet", "hotspot"],
        ["compartir internet", "punto de acceso", "hotspot"]
    ),
    page!(
        "airplane",
        "ms-settings:network-airplanemode",
        "Airplane mode",
        "Modo avión",
        ["flight mode", "wireless off"],
        ["modo vuelo"]
    ),
    page!(
        "networkadvanced",
        "ms-settings:network-advancedsettings",
        "Advanced network settings",
        "Configuración de red avanzada",
        ["adapters", "network reset", "dns", "ip address"],
        ["adaptadores", "restablecer red", "dns", "dirección ip"]
    ),
    // Personalization
    page!(
        "personalization",
        "ms-settings:personalization",
        "Personalization",
        "Personalización",
        ["customize", "appearance"],
        ["personalizar", "apariencia"]
    ),
    page!(
        "background",
        "ms-settings:personalization-background",
        "Background",
        "Fondo",
        ["wallpaper", "desktop background", "slideshow"],
        ["fondo de pantalla", "fondo de escritorio", "presentación"]
    ),
    page!(
        "colors",
        "ms-settings:colors",
        "Colors",
        "Colores",
        [
            "dark mode",
            "light mode",
            "accent color",
            "theme color",
            "transparency"
        ],
        [
            "modo oscuro",
            "modo claro",
            "color de énfasis",
            "color de acento",
            "transparencia"
        ]
    ),
    page!(
        "themes",
        "ms-settings:themes",
        "Themes",
        "Temas",
        ["theme", "desktop icons", "cursor"],
        ["tema", "iconos del escritorio"]
    ),
    page!(
        "lockscreen",
        "ms-settings:lockscreen",
        "Lock screen",
        "Pantalla de bloqueo",
        ["lock screen picture", "spotlight", "screen saver"],
        [
            "imagen de bloqueo",
            "contenido destacado",
            "protector de pantalla"
        ]
    ),
    page!(
        "start",
        "ms-settings:personalization-start",
        "Start",
        "Inicio",
        ["start menu", "recommended", "recent files"],
        ["menú inicio", "recomendado", "archivos recientes"]
    ),
    page!(
        "taskbar",
        "ms-settings:taskbar",
        "Taskbar",
        "Barra de tareas",
        [
            "taskbar alignment",
            "system tray",
            "notification area",
            "hide taskbar"
        ],
        [
            "alineación",
            "bandeja del sistema",
            "área de notificación",
            "ocultar barra de tareas"
        ]
    ),
    page!(
        "fonts",
        "ms-settings:fonts",
        "Fonts",
        "Fuentes",
        ["typeface", "install fonts"],
        ["tipografía", "tipos de letra", "instalar fuentes"]
    ),
    // Apps
    page!(
        "appsfeatures",
        "ms-settings:appsfeatures",
        "Installed apps",
        "Aplicaciones instaladas",
        ["apps & features", "uninstall", "programs", "remove app"],
        [
            "aplicaciones y características",
            "desinstalar",
            "programas",
            "quitar aplicación"
        ]
    ),
    page!(
        "defaultapps",
        "ms-settings:defaultapps",
        "Default apps",
        "Aplicaciones predeterminadas",
        ["default browser", "file associations", "open with"],
        [
            "navegador predeterminado",
            "asociaciones de archivos",
            "abrir con"
        ]
    ),
    page!(
        "startupapps",
        "ms-settings:startupapps",
        "Startup apps",
        "Aplicaciones de inicio",
        ["startup", "autostart", "run at login"],
        ["inicio", "arranque", "inicio automático"]
    ),
    // Accounts
    page!(
        "accounts",
        "ms-settings:accounts",
        "Accounts",
        "Cuentas",
        ["account", "user", "microsoft account"],
        ["cuenta", "usuario", "cuenta microsoft"]
    ),
    page!(
        "yourinfo",
        "ms-settings:yourinfo",
        "Your info",
        "Tu información",
        ["profile picture", "account picture"],
        ["foto de perfil", "imagen de la cuenta"]
    ),
    page!(
        "signin",
        "ms-settings:signinoptions",
        "Sign-in options",
        "Opciones de inicio de sesión",
        [
            "password",
            "pin",
            "windows hello",
            "fingerprint",
            "face",
            "dynamic lock"
        ],
        [
            "contraseña",
            "pin",
            "windows hello",
            "huella digital",
            "reconocimiento facial",
            "bloqueo dinámico"
        ]
    ),
    page!(
        "email",
        "ms-settings:emailandaccounts",
        "Email & accounts",
        "Correo electrónico y cuentas",
        ["mail", "work account"],
        ["correo", "cuenta profesional"]
    ),
    page!(
        "otherusers",
        "ms-settings:otherusers",
        "Other users",
        "Otros usuarios",
        ["family", "add user", "guest"],
        ["familia", "agregar usuario", "invitado"]
    ),
    page!(
        "backup",
        "ms-settings:backup",
        "Windows Backup",
        "Copia de seguridad de Windows",
        ["backup", "onedrive", "sync"],
        ["copia de seguridad", "respaldo", "sincronizar"]
    ),
    // Time & language
    page!(
        "datetime",
        "ms-settings:dateandtime",
        "Date & time",
        "Fecha y hora",
        ["clock", "time zone", "sync time"],
        ["reloj", "zona horaria", "sincronizar hora"]
    ),
    page!(
        "region",
        "ms-settings:regionlanguage",
        "Language & region",
        "Idioma y región",
        [
            "language",
            "region",
            "country",
            "display language",
            "format"
        ],
        ["idioma", "región", "país", "formato"]
    ),
    page!(
        "keyboard",
        "ms-settings:keyboard",
        "Keyboard",
        "Teclado",
        ["input method", "keyboard layout", "input language"],
        ["método de entrada", "distribución de teclado"]
    ),
    page!(
        "speech",
        "ms-settings:speech",
        "Speech",
        "Voz",
        ["voice", "speech recognition", "dictation"],
        ["reconocimiento de voz", "dictado"]
    ),
    // Gaming
    page!(
        "gamemode",
        "ms-settings:gaming-gamemode",
        "Game Mode",
        "Modo de juego",
        ["gaming", "games", "game bar"],
        ["juegos", "jugar", "barra de juego"]
    ),
    // Accessibility
    page!(
        "accessibility",
        "ms-settings:easeofaccess",
        "Accessibility",
        "Accesibilidad",
        ["ease of access", "a11y"],
        ["facilidad de acceso"]
    ),
    page!(
        "textsize",
        "ms-settings:easeofaccess-display",
        "Text size",
        "Tamaño del texto",
        ["font size", "bigger text", "visual effects"],
        ["tamaño de fuente", "texto más grande", "efectos visuales"]
    ),
    page!(
        "magnifier",
        "ms-settings:easeofaccess-magnifier",
        "Magnifier",
        "Lupa",
        ["zoom", "magnify"],
        ["ampliar", "zoom"]
    ),
    page!(
        "narrator",
        "ms-settings:easeofaccess-narrator",
        "Narrator",
        "Narrador",
        ["screen reader", "read aloud"],
        ["lector de pantalla", "leer en voz alta"]
    ),
    page!(
        "contrast",
        "ms-settings:easeofaccess-highcontrast",
        "Contrast themes",
        "Temas de contraste",
        ["high contrast"],
        ["contraste alto"]
    ),
    // Privacy & security
    page!(
        "privacy",
        "ms-settings:privacy",
        "Privacy & security",
        "Privacidad y seguridad",
        ["permissions", "app permissions", "diagnostics"],
        ["permisos", "permisos de aplicaciones", "diagnósticos"]
    ),
    page!(
        "location",
        "ms-settings:privacy-location",
        "Location",
        "Ubicación",
        ["gps", "location services"],
        ["gps", "servicios de ubicación"]
    ),
    page!(
        "privacycamera",
        "ms-settings:privacy-webcam",
        "Camera permissions",
        "Permisos de la cámara",
        ["webcam access"],
        ["acceso a la cámara"]
    ),
    page!(
        "microphone",
        "ms-settings:privacy-microphone",
        "Microphone permissions",
        "Permisos del micrófono",
        ["mic access"],
        ["acceso al micrófono"]
    ),
    page!(
        "findmydevice",
        "ms-settings:findmydevice",
        "Find my device",
        "Encontrar mi dispositivo",
        ["lost device", "locate"],
        ["dispositivo perdido", "localizar"]
    ),
    page!(
        "developers",
        "ms-settings:developers",
        "For developers",
        "Para programadores",
        ["developer mode", "dev mode", "sudo"],
        ["modo de desarrollador", "desarrolladores"]
    ),
    page!(
        "security",
        "windowsdefender:",
        "Windows Security",
        "Seguridad de Windows",
        ["antivirus", "defender", "firewall", "virus", "malware"],
        ["antivirus", "defender", "firewall", "virus", "malware"]
    ),
    // Windows Update
    page!(
        "windowsupdate",
        "ms-settings:windowsupdate",
        "Windows Update",
        "Windows Update",
        ["updates", "check for updates", "upgrade", "patch"],
        ["actualizaciones", "buscar actualizaciones", "actualizar"]
    ),
    page!(
        "updatehistory",
        "ms-settings:windowsupdate-history",
        "Update history",
        "Historial de actualizaciones",
        ["installed updates", "uninstall updates"],
        ["actualizaciones instaladas", "desinstalar actualizaciones"]
    ),
    // Classic tools
    entry!(
        Tool,
        "controlpanel",
        "control.exe",
        "",
        "Control Panel",
        "Panel de control",
        ["control"],
        ["panel"]
    ),
    entry!(
        Tool,
        "windowstools",
        "control.exe",
        "/name Microsoft.AdministrativeTools",
        "Windows Tools",
        "Herramientas de Windows",
        ["administrative tools", "admin tools"],
        ["herramientas administrativas"]
    ),
    entry!(
        Tool,
        "devmgmt",
        "devmgmt.msc",
        "",
        "Device Manager",
        "Administrador de dispositivos",
        ["drivers", "hardware", "devmgmt"],
        ["controladores", "hardware", "devmgmt"]
    ),
    entry!(
        Tool,
        "diskmgmt",
        "diskmgmt.msc",
        "",
        "Disk Management",
        "Administración de discos",
        ["partitions", "format drive", "volumes", "diskmgmt"],
        ["particiones", "formatear unidad", "volúmenes", "diskmgmt"]
    ),
    entry!(
        Tool,
        "services",
        "services.msc",
        "",
        "Services",
        "Servicios",
        ["windows services", "services.msc"],
        ["servicios de windows"]
    ),
    entry!(
        Tool,
        "eventvwr",
        "eventvwr.msc",
        "",
        "Event Viewer",
        "Visor de eventos",
        ["logs", "events", "eventvwr"],
        ["registros", "eventos", "eventvwr"]
    ),
    entry!(
        Tool,
        "compmgmt",
        "compmgmt.msc",
        "",
        "Computer Management",
        "Administración de equipos",
        ["compmgmt", "users and groups"],
        ["compmgmt", "usuarios y grupos"]
    ),
    entry!(
        Tool,
        "taskschd",
        "taskschd.msc",
        "",
        "Task Scheduler",
        "Programador de tareas",
        ["scheduled tasks", "cron"],
        ["tareas programadas"]
    ),
    entry!(
        Tool,
        "regedit",
        "regedit.exe",
        "",
        "Registry Editor",
        "Editor del Registro",
        ["regedit", "registry"],
        ["regedit", "registro"]
    ),
    entry!(
        Tool,
        "taskmgr",
        "taskmgr.exe",
        "",
        "Task Manager",
        "Administrador de tareas",
        ["processes", "taskmgr", "performance", "end task"],
        ["procesos", "taskmgr", "rendimiento", "finalizar tarea"]
    ),
    entry!(
        Tool,
        "resmon",
        "resmon.exe",
        "",
        "Resource Monitor",
        "Monitor de recursos",
        ["resmon", "performance"],
        ["resmon", "rendimiento"]
    ),
    entry!(
        Tool,
        "msconfig",
        "msconfig.exe",
        "",
        "System Configuration",
        "Configuración del sistema",
        ["msconfig", "boot"],
        ["msconfig", "arranque"]
    ),
    entry!(
        Tool,
        "msinfo32",
        "msinfo32.exe",
        "",
        "System Information",
        "Información del sistema",
        ["msinfo32", "hardware info"],
        ["msinfo32", "información de hardware"]
    ),
    entry!(
        Tool,
        "sysprops",
        "SystemPropertiesAdvanced.exe",
        "",
        "Advanced system settings",
        "Configuración avanzada del sistema",
        ["system properties", "virtual memory", "performance options"],
        [
            "propiedades del sistema",
            "memoria virtual",
            "opciones de rendimiento"
        ]
    ),
    entry!(
        Tool,
        "envvars",
        "rundll32.exe",
        "sysdm.cpl,EditEnvironmentVariables",
        "Edit environment variables",
        "Editar las variables de entorno",
        ["environment variables", "path", "env"],
        ["variables de entorno", "path"]
    ),
    entry!(
        Tool,
        "ncpa",
        "ncpa.cpl",
        "",
        "Network Connections",
        "Conexiones de red",
        ["network adapters", "ncpa"],
        ["adaptadores de red", "ncpa"]
    ),
    entry!(
        Tool,
        "appwiz",
        "appwiz.cpl",
        "",
        "Programs and Features",
        "Programas y características",
        ["uninstall a program", "appwiz"],
        ["desinstalar un programa", "appwiz"]
    ),
    entry!(
        Tool,
        "cmd",
        "cmd.exe",
        "",
        "Command Prompt",
        "Símbolo del sistema",
        ["cmd", "console", "terminal", "shell"],
        ["cmd", "consola", "terminal"]
    ),
    entry!(
        Tool,
        "powershell",
        "powershell.exe",
        "",
        "Windows PowerShell",
        "Windows PowerShell",
        ["powershell", "ps", "shell", "console"],
        ["consola"]
    ),
    entry!(
        Tool,
        "terminal",
        "wt.exe",
        "",
        "Terminal",
        "Terminal",
        ["windows terminal", "wt", "console", "shell"],
        ["consola"],
        requires = "wt.exe"
    ),
];

/// One search hit: index into [`ENTRIES`] and its score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsHit {
    pub index: usize,
    pub score: u32,
}

impl SettingsHit {
    pub fn entry(&self) -> &'static SettingsEntry {
        &ENTRIES[self.index]
    }
}

/// Search the table. `available` filters entries (e.g. tools whose
/// `requires` program is missing). Best first, at most `limit`.
pub fn search(
    searcher: &mut Searcher,
    query: &str,
    lang: Lang,
    available: impl Fn(&SettingsEntry) -> bool,
    limit: usize,
) -> Vec<SettingsHit> {
    let Some(q) = Query::new(query) else {
        return Vec::new();
    };
    let mut hits: Vec<SettingsHit> = ENTRIES
        .iter()
        .enumerate()
        .filter(|(_, e)| available(e))
        .filter_map(|(index, e)| {
            searcher
                .score(&q, e.name(lang), e.keywords(lang))
                .map(|score| SettingsHit { index, score })
        })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.entry().name(lang).len().cmp(&b.entry().name(lang).len()))
    });
    hits.truncate(limit);
    hits
}

/// Look an entry up by id.
pub fn by_id(id: &str) -> Option<&'static SettingsEntry> {
    ENTRIES.iter().find(|e| e.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn top(q: &str, lang: Lang) -> &'static str {
        let mut s = Searcher::new();
        search(&mut s, q, lang, |_| true, 5)
            .first()
            .map(|h| h.entry().id)
            .unwrap_or("")
    }

    #[test]
    fn table_is_well_formed() {
        assert!(ENTRIES.len() >= 60, "{}", ENTRIES.len());
        let mut ids: Vec<&str> = ENTRIES.iter().map(|e| e.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), ENTRIES.len(), "duplicate ids");
        for e in ENTRIES {
            assert!(!e.name_en.is_empty() && !e.name_es.is_empty(), "{}", e.id);
            assert!(!e.target.is_empty(), "{}", e.id);
            if e.kind == EntryKind::Settings {
                assert!(
                    e.target.starts_with("ms-settings:") || e.target.ends_with(':'),
                    "{}",
                    e.id
                );
            }
            // Never anything that reaches the web.
            assert!(!crate::run_command::mentions_url(e.target) || e.target.ends_with(':'));
            assert!(!e.target.contains("http"), "{}", e.id);
        }
    }

    #[test]
    fn english_queries() {
        assert_eq!(top("display", Lang::En), "display");
        assert_eq!(top("brightness", Lang::En), "display");
        assert_eq!(top("wifi", Lang::En), "wifi");
        assert_eq!(top("bluetooth", Lang::En), "bluetooth");
        assert_eq!(top("windows update", Lang::En), "windowsupdate");
        assert_eq!(top("device manager", Lang::En), "devmgmt");
        assert_eq!(top("wallpaper", Lang::En), "background");
        assert_eq!(top("vpn", Lang::En), "vpn");
    }

    #[test]
    fn spanish_queries_with_and_without_accents() {
        assert_eq!(top("pantalla", Lang::Es), "display");
        assert_eq!(top("fondo de pantalla", Lang::Es), "background");
        assert_eq!(top("energia", Lang::Es), "power");
        assert_eq!(top("administrador de dispositivos", Lang::Es), "devmgmt");
        assert_eq!(top("barra de tareas", Lang::Es), "taskbar");
        assert_eq!(top("desinstalar", Lang::Es), "appsfeatures");
        // The other language still works.
        assert_eq!(top("taskbar", Lang::Es), "taskbar");
    }

    #[test]
    fn filtering_and_lookup() {
        let mut s = Searcher::new();
        let hits = search(&mut s, "terminal", Lang::En, |e| e.requires.is_none(), 10);
        assert!(hits.iter().all(|h| h.entry().id != "terminal"));
        assert!(search(&mut s, "", Lang::En, |_| true, 10).is_empty());
        assert_eq!(by_id("regedit").map(|e| e.target), Some("regedit.exe"));
        assert_eq!(by_id("display").unwrap().name(Lang::Es), "Pantalla");
        assert!(by_id("nope").is_none());
    }
}
