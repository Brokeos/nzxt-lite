//! Service résident : icône dans la zone de notification, réapplication de la configuration
//! (modification de config.toml, reconnexion de la Kraken, sortie de veille) et, en mode
//! Infographie, dessin de l'écran — l'image n'est renvoyée que si un entier affiché change.
//!
//! Un seul thread : une fenêtre cachée reçoit les messages de l'icône et un `WM_TIMER` par seconde.

use crate::config::{self, Config, Lcd, LcdMode};
use crate::kraken::Kraken;
use crate::render::{self, Fonts, Values};
use crate::sensors::Sensors;
use crate::system;
use std::cell::{Cell, RefCell};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant, SystemTime};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Threading::{CreateMutexW, OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW,
    FindWindowW, GetCursorPos, GetMessageW, GetSystemMetrics, HICON, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG,
    PBT_APMRESUMEAUTOMATIC, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SM_CXSMICON,
    SetForegroundWindow, SetMenuDefaultItem, SetTimer, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
    TranslateMessage, WM_APP, WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NULL,
    WM_POWERBROADCAST, WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};

const MUTEX_NAME: &str = r"Local\nzxt-lite-daemon";
const WINDOW_CLASS: &str = "NzxtLiteTray";
const TICK_MS: u32 = 1000;
/// Délai entre deux tentatives de connexion, multiplié par le nombre d'échecs (plafonné).
const RETRY: Duration = Duration::from_secs(3);
const MAX_BACKOFF: u32 = 10;
/// Image renvoyée au moins à cet intervalle même si les valeurs ne changent pas.
const REFRESH: Duration = Duration::from_secs(30);
/// Un double-clic produit deux clics : on n'ouvre la fenêtre qu'une fois.
const OPEN_DEBOUNCE: Duration = Duration::from_millis(1500);

const WM_TRAY: u32 = WM_APP + 1;
const ID_OPEN: usize = 1;
const ID_AUTOSTART: usize = 2;
const ID_QUIT: usize = 3;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn is_running() -> bool {
    let h = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, wide(MUTEX_NAME).as_ptr()) };
    if h.is_null() {
        return false;
    }
    unsafe { CloseHandle(h) };
    true
}

/// Lance le service en arrière-plan (processus détaché, sans console).
pub fn spawn() -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const DETACHED: u32 = 0x0000_0008 | 0x0800_0000; // DETACHED_PROCESS | CREATE_NO_WINDOW
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(exe).arg("daemon").creation_flags(DETACHED).spawn().map(drop).map_err(|e| e.to_string())
}

/// Demande au service de se fermer (comme « Quitter » dans le menu de l'icône).
pub fn stop() -> Result<(), String> {
    let hwnd = unsafe { FindWindowW(wide(WINDOW_CLASS).as_ptr(), null()) };
    if hwnd.is_null() {
        return Err("service introuvable".into());
    }
    unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
    Ok(())
}

fn config_mtime() -> Option<SystemTime> {
    std::fs::metadata(config::path()).and_then(|m| m.modified()).ok()
}

fn fmt_temp(v: Option<f32>) -> String {
    v.map_or_else(|| "n/d".into(), |t| format!("{t:.0} °C"))
}

/// État du service, propriétaire unique de la Kraken dans ce processus.
struct Service {
    cfg: Config,
    mtime: Option<SystemTime>,
    sensors: Sensors,
    /// Polices chargées seulement en mode Infographie.
    fonts: Option<Fonts>,
    kraken: Option<Kraken>,
    failures: u32,
    retry_at: Instant,
    /// Réglages écran déjà envoyés : une image/un GIF n'est renvoyé que s'ils changent
    /// ou si la Kraken a été réinitialisée (débranchement, veille).
    uploaded: Option<Lcd>,
    values: Values,
    last_key: Option<[Option<i32>; 3]>,
    last_push: Instant,
    error: Option<String>,
    tip: String,
}

impl Service {
    fn new(cfg: Config) -> Self {
        let now = Instant::now();
        Self {
            cfg,
            mtime: config_mtime(),
            sensors: Sensors::new(),
            fonts: None,
            kraken: None,
            failures: 0,
            retry_at: now,
            uploaded: None,
            values: Values::default(),
            last_key: None,
            last_push: now,
            error: None,
            tip: String::new(),
        }
    }

    /// La Kraken a pu être réinitialisée : tout renvoyer, image comprise.
    fn reset(&mut self) {
        self.kraken = None;
        self.uploaded = None;
        self.retry_at = Instant::now();
    }

    fn connect(&mut self) {
        let upload = self.cfg.lcd.mode == LcdMode::Image && self.uploaded.as_ref() != Some(&self.cfg.lcd);
        let result = Kraken::open().map_err(|e| e.to_string()).and_then(|mut k| self.cfg.apply(&mut k, upload).map(|()| k));
        match result {
            Ok(k) => {
                self.kraken = Some(k);
                self.uploaded = Some(self.cfg.lcd.clone());
                self.failures = 0;
                self.last_key = None;
                self.error = None;
            }
            Err(e) => {
                self.failures = (self.failures + 1).min(MAX_BACKOFF);
                self.retry_at = Instant::now() + RETRY * self.failures;
                self.error = Some(e);
            }
        }
    }

    /// Une seconde : config, connexion, capteurs, écran. Renvoie l'infobulle si elle a changé.
    fn tick(&mut self) -> Option<String> {
        let m = config_mtime();
        if m != self.mtime {
            self.mtime = m;
            // Config invalide (écriture en cours, faute de frappe) : on garde la précédente.
            if let Some(c) = Config::load().ok().filter(|c| *c != self.cfg) {
                self.cfg = c;
                // On rouvre et réapplique tout ; l'image n'est renvoyée que si l'écran a changé.
                self.kraken = None;
                self.failures = 0;
                self.retry_at = Instant::now();
            }
        }
        if self.kraken.is_none() && Instant::now() >= self.retry_at {
            self.connect();
        }
        let liquid = self.kraken.as_ref().and_then(|k| match k.status() {
            Ok(s) => Some(s.liquid_c),
            Err(e) => {
                self.error = Some(e.to_string());
                None
            }
        });
        if self.kraken.is_some() && liquid.is_none() {
            self.reset();
        }
        let r = self.sensors.read();
        self.values = Values { cpu: r.cpu_temp, gpu: r.gpu_temp, liquid };
        if self.cfg.lcd.mode == LcdMode::Infographic {
            self.draw();
        } else {
            self.fonts = None;
        }
        let tip = self.tooltip();
        (tip != self.tip).then(|| {
            self.tip = tip.clone();
            tip
        })
    }

    fn draw(&mut self) {
        let Some(k) = self.kraken.as_mut() else { return };
        let key = self.values.key();
        if Some(key) == self.last_key && self.last_push.elapsed() < REFRESH {
            return;
        }
        let fonts = self.fonts.get_or_insert_with(Fonts::load);
        let img = render::infographic(fonts, &self.cfg.lcd, &self.values);
        let orientation = (self.cfg.lcd.rotation / 90 % 4) as u8;
        match k.push_frame(render::to_device(&img, orientation)) {
            Ok(()) => {
                self.last_key = Some(key);
                self.last_push = Instant::now();
            }
            Err(e) => {
                self.error = Some(e.to_string());
                self.reset();
            }
        }
    }

    /// Infobulle de l'icône (128 caractères au plus).
    fn tooltip(&self) -> String {
        let v = &self.values;
        let mut tip = match (&self.kraken, &self.error) {
            (Some(_), _) => format!("NZXT Lite\nLiquide {}\nCPU {} · GPU {}", fmt_temp(v.liquid), fmt_temp(v.cpu), fmt_temp(v.gpu)),
            (None, Some(e)) => format!("NZXT Lite\n{e}"),
            (None, None) => "NZXT Lite\nKraken non connectée".into(),
        };
        if tip.encode_utf16().count() > 127 {
            tip = tip.chars().take(120).collect::<String>() + "…";
        }
        tip
    }

    /// À la fermeture : en mode Infographie, on rend l'écran au firmware plutôt que de
    /// laisser une image figée avec des températures périmées.
    fn shutdown(&mut self) {
        if let Some(k) = self.kraken.take() {
            if self.cfg.lcd.mode == LcdMode::Infographic {
                let _ = k.show_liquid();
            }
        }
    }
}

thread_local! {
    static SERVICE: RefCell<Option<Service>> = const { RefCell::new(None) };
    static ICON: Cell<HICON> = const { Cell::new(null_mut()) };
    static ICON_SHOWN: Cell<bool> = const { Cell::new(false) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
    static LAST_OPEN: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Accès au service hors réentrance (un menu ouvert fait tourner une boucle de messages).
fn with_service<R>(f: impl FnOnce(&mut Service) -> R) -> Option<R> {
    SERVICE.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

fn notify(hwnd: HWND, action: u32, tip: &str) -> bool {
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = ICON.get();
    for (dst, src) in nid.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
        *dst = src;
    }
    unsafe { Shell_NotifyIconW(action, &nid) != 0 }
}

fn show_icon(hwnd: HWND) {
    let tip = with_service(|s| s.tooltip()).unwrap_or_else(|| "NZXT Lite".into());
    ICON_SHOWN.set(notify(hwnd, NIM_ADD, &tip));
}

fn open_gui() {
    if LAST_OPEN.get().is_some_and(|t| t.elapsed() < OPEN_DEBOUNCE) {
        return;
    }
    LAST_OPEN.set(Some(Instant::now()));
    let _ = system::open_gui();
}

fn show_menu(hwnd: HWND) {
    let status = with_service(|s| {
        let v = s.values;
        if s.kraken.is_some() {
            format!("Liquide {} · CPU {} · GPU {}", fmt_temp(v.liquid), fmt_temp(v.cpu), fmt_temp(v.gpu))
        } else {
            "Kraken non connectée".into()
        }
    });
    unsafe {
        let menu = CreatePopupMenu();
        if let Some(status) = status {
            AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, wide(&status).as_ptr());
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
        }
        AppendMenuW(menu, MF_STRING, ID_OPEN, wide("Ouvrir NZXT Lite").as_ptr());
        SetMenuDefaultItem(menu, ID_OPEN as u32, 0);
        let checked = if system::autostart_enabled() { MF_CHECKED } else { 0 };
        AppendMenuW(menu, MF_STRING | checked, ID_AUTOSTART, wide("Lancer au démarrage").as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        AppendMenuW(menu, MF_STRING, ID_QUIT, wide("Quitter").as_ptr());
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        // Sans premier plan, le menu ne se fermerait pas en cliquant ailleurs.
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, 0, hwnd, null());
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        if cmd > 0 {
            command(hwnd, cmd as usize);
        }
    }
}

fn command(hwnd: HWND, id: usize) {
    match id {
        ID_OPEN => open_gui(),
        ID_AUTOSTART => {
            let _ = system::set_autostart(!system::autostart_enabled());
        }
        ID_QUIT => unsafe {
            DestroyWindow(hwnd);
        },
        _ => {}
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            if !ICON_SHOWN.get() {
                show_icon(hwnd); // Explorer pas encore prêt au démarrage de session
            }
            if let Some(Some(tip)) = with_service(Service::tick) {
                notify(hwnd, NIM_MODIFY, &tip);
            }
        }
        WM_TRAY => match lparam as u32 {
            WM_LBUTTONUP | WM_LBUTTONDBLCLK => open_gui(),
            WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
            _ => {}
        },
        WM_COMMAND => command(hwnd, wparam & 0xFFFF),
        WM_POWERBROADCAST => {
            if wparam == PBT_APMRESUMEAUTOMATIC as usize {
                with_service(Service::reset);
            }
            return 1;
        }
        WM_DESTROY => {
            with_service(Service::shutdown);
            notify(hwnd, NIM_DELETE, "");
            unsafe { PostQuitMessage(0) };
        }
        // Explorer redémarré : l'icône a disparu de la zone de notification.
        m if m != 0 && m == TASKBAR_CREATED.get() => show_icon(hwnd),
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    0
}

/// Menus contextuels sombres si Windows est en thème sombre (uxtheme, non documenté,
/// Windows 10 1903+ ; sans effet ailleurs).
fn follow_system_theme() {
    unsafe {
        let lib = LoadLibraryW(wide("uxtheme.dll").as_ptr());
        if lib.is_null() {
            return;
        }
        // Ordinal 135 : SetPreferredAppMode(AllowDark = 1).
        if let Some(f) = GetProcAddress(lib, 135 as *const u8) {
            let set_mode: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            set_mode(1);
        }
    }
}

pub fn run() -> Result<(), String> {
    let mutex = unsafe { CreateMutexW(null(), 0, wide(MUTEX_NAME).as_ptr()) };
    if mutex.is_null() || unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return Ok(()); // déjà lancé
    }
    let cfg = Config::load()?;
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    follow_system_theme();
    SERVICE.set(Some(Service::new(cfg)));
    ICON.set(system::load_icon(unsafe { GetSystemMetrics(SM_CXSMICON) }));
    TASKBAR_CREATED.set(unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) });

    let class = wide(WINDOW_CLASS);
    let hwnd = unsafe {
        let instance = GetModuleHandleW(null());
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance, lpszClassName: class.as_ptr(), ..std::mem::zeroed() };
        RegisterClassW(&wc);
        // Fenêtre de premier niveau jamais affichée : elle reçoit les diffusions
        // (sortie de veille, redémarrage d'Explorer), contrairement à HWND_MESSAGE.
        CreateWindowExW(WS_EX_TOOLWINDOW, class.as_ptr(), class.as_ptr(), WS_POPUP, 0, 0, 0, 0, null_mut(), null_mut(), instance, null())
    };
    if hwnd.is_null() {
        unsafe { CloseHandle(mutex) };
        return Err("création de la fenêtre du service impossible".into());
    }
    with_service(Service::tick);
    show_icon(hwnd);
    unsafe {
        SetTimer(hwnd, 1, TICK_MS, None);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        CloseHandle(mutex);
    }
    Ok(())
}
