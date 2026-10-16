//! Intégration Windows : démarrage automatique, fenêtre principale, icône et détection de NZXT CAM.

use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HWND, INVALID_HANDLE_VALUE, LPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowThreadProcessId, HICON, ICON_BIG, ICON_SMALL, IMAGE_ICON, IsIconic,
    IsWindowVisible, LR_DEFAULTCOLOR, LoadImageW, SM_CXICON, SM_CXSMICON, SW_RESTORE, SendMessageW, SetForegroundWindow,
    ShowWindow, WM_SETICON,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "nzxt-lite";
const CAM_PROCESS: &str = "NZXT CAM.exe";
/// Entrée de démarrage créée par l'installeur de NZXT CAM.
const CAM_RUN_VALUE: &str = "NZXT.CAM";
/// Titre de la fenêtre de réglages (sert aussi à la retrouver depuis un autre processus).
pub const GUI_TITLE: &str = "NZXT Lite";
/// Identifiant de l'icône dans les ressources (assets/nzxt-lite.rc).
const ICON_ID: u16 = 1;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn autostart_command() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(format!("\"{}\" daemon", exe.display()))
}

pub fn autostart_enabled() -> bool {
    let (key, value) = (wide(RUN_KEY), wide(RUN_VALUE));
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let rc = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, null_mut(), buf.as_mut_ptr().cast(), &mut size)
    };
    if rc != ERROR_SUCCESS {
        return false;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    autostart_command().is_ok_and(|cmd| String::from_utf16_lossy(&buf[..len]) == cmd)
}

pub fn set_autostart(enabled: bool) -> Result<(), String> {
    let (key, value) = (wide(RUN_KEY), wide(RUN_VALUE));
    let rc = if enabled {
        let data = wide(&autostart_command()?);
        unsafe {
            RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), REG_SZ, data.as_ptr().cast(), (data.len() * 2) as u32)
        }
    } else {
        match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) } {
            2 => ERROR_SUCCESS, // ERROR_FILE_NOT_FOUND : déjà absent
            rc => rc,
        }
    };
    if rc == ERROR_SUCCESS { Ok(()) } else { Err(format!("registre : erreur {rc}")) }
}

/// NZXT CAM se relance-t-il à l'ouverture de session (et reprend-il la Kraken) ?
pub fn cam_autostart() -> bool {
    let (key, value) = (wide(RUN_KEY), wide(CAM_RUN_VALUE));
    let rc = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, null_mut(), null_mut(), null_mut()) };
    rc == ERROR_SUCCESS
}

/// Retire NZXT CAM du démarrage (uniquement à la demande de l'utilisateur).
pub fn remove_cam_autostart() -> Result<(), String> {
    let (key, value) = (wide(RUN_KEY), wide(CAM_RUN_VALUE));
    match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) } {
        ERROR_SUCCESS | 2 => Ok(()),
        rc => Err(format!("registre : erreur {rc}")),
    }
}

/// PIDs des processus NZXT CAM en cours.
pub fn cam_processes() -> Vec<u32> {
    let mut pids = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return pids;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut entry);
        while ok != 0 {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            if String::from_utf16_lossy(&entry.szExeFile[..len]).eq_ignore_ascii_case(CAM_PROCESS) {
                pids.push(entry.th32ProcessID);
            }
            ok = Process32NextW(snap, &mut entry);
        }
        CloseHandle(snap);
    }
    pids
}

/// Ferme NZXT CAM (demandé explicitement par l'utilisateur depuis l'interface).
pub fn close_cam() -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let status = std::process::Command::new("taskkill")
        .args(["/IM", CAM_PROCESS, "/T", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err("impossible de fermer NZXT CAM (droits admin ?)".into()) }
}

fn exe_of(pid: u32) -> Option<std::path::PathBuf> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) != 0;
        CloseHandle(h);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]).into())
    }
}

/// Fenêtre de réglages ouverte par ce même exécutable (`pid` : restreint à ce processus).
pub fn find_gui_window(pid: Option<u32>) -> Option<HWND> {
    struct Search {
        pid: Option<u32>,
        exe: Option<std::path::PathBuf>,
        found: Option<HWND>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> i32 {
        let s = unsafe { &mut *(lparam as *mut Search) };
        let mut title = [0u16; 32];
        let n = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
        if n <= 0 || String::from_utf16_lossy(&title[..n as usize]) != GUI_TITLE {
            return 1;
        }
        let mut owner = 0;
        unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
        let ours = match s.pid {
            Some(pid) => owner == pid,
            None => s.exe.is_some() && exe_of(owner) == s.exe,
        };
        if ours {
            s.found = Some(hwnd);
            return 0;
        }
        1
    }
    let mut s = Search { pid, exe: std::env::current_exe().ok(), found: None };
    unsafe { EnumWindows(Some(visit), (&raw mut s) as LPARAM) };
    s.found
}

/// Ramène la fenêtre de réglages au premier plan ; `false` si elle n'est pas ouverte.
pub fn focus_gui() -> bool {
    let Some(hwnd) = find_gui_window(None) else { return false };
    unsafe {
        if IsIconic(hwnd) != 0 || IsWindowVisible(hwnd) == 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd);
    }
    true
}

/// Ouvre la fenêtre de réglages (nouveau processus) ou la ramène devant si elle existe.
pub fn open_gui() -> Result<(), String> {
    if focus_gui() {
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(exe).spawn().map(drop).map_err(|e| e.to_string())
}

/// Icône de l'application à la taille demandée (pixels), depuis les ressources de l'exe.
pub fn load_icon(size: i32) -> HICON {
    unsafe { LoadImageW(GetModuleHandleW(null()), ICON_ID as usize as *const u16, IMAGE_ICON, size, size, LR_DEFAULTCOLOR) }
}

/// Pose l'icône de l'exe sur une fenêtre (barre de titre et barre des tâches), net quel que soit le DPI.
pub fn set_window_icons(hwnd: HWND) {
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        for (kind, metric) in [(ICON_SMALL, SM_CXSMICON), (ICON_BIG, SM_CXICON)] {
            let icon = load_icon(GetSystemMetricsForDpi(metric, dpi));
            if !icon.is_null() {
                SendMessageW(hwnd, WM_SETICON, kind as usize, icon as isize);
            }
        }
    }
}

/// Projette un fichier en mémoire, en lecture seule, pour toute la durée du processus.
/// Les pages appartiennent au fichier (partageables) : elles ne comptent pas en mémoire privée,
/// contrairement à une lecture dans un `Vec` — utile pour les polices de plusieurs Mo.
pub fn map_file(path: &str) -> Option<&'static [u8]> {
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, GetFileSizeEx, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Memory::{CreateFileMappingW, FILE_MAP_READ, MapViewOfFile, PAGE_READONLY};
    let path = wide(path);
    unsafe {
        let file = CreateFileW(path.as_ptr(), windows_sys::Win32::Foundation::GENERIC_READ, FILE_SHARE_READ, null(), OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, null_mut());
        if file == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut size = 0i64;
        let mapping = if GetFileSizeEx(file, &mut size) != 0 && size > 0 {
            CreateFileMappingW(file, null(), PAGE_READONLY, 0, 0, null())
        } else {
            null_mut()
        };
        CloseHandle(file);
        if mapping.is_null() {
            return None;
        }
        // La vue garde la projection vivante ; elle n'est jamais libérée (durée de vie 'static).
        let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0);
        CloseHandle(mapping);
        (!view.Value.is_null()).then(|| std::slice::from_raw_parts(view.Value as *const u8, size as usize))
    }
}
