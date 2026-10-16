//! Capteurs CPU (AMD Zen via le pilote WinRing0 déjà installé) et GPU (API noyau D3DKMT,
//! comme le Gestionnaire des tâches : aucune DLL constructeur chargée).
//! Aucun pilote n'est installé par nzxt-lite : si une source manque, la valeur est `None`.

use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{
    CloseHandle, FILETIME, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Threading::{CreateMutexW, GetSystemTimes, ReleaseMutex, WaitForSingleObject};

const IOCTL_READ_PCI_CONFIG: u32 = 0x9C40_6144;
const IOCTL_WRITE_PCI_CONFIG: u32 = 0x9C40_A148;
/// Registre SMN THM_TCON_CUR_TMP (Zen / Zen2 / Zen3 / Zen4).
const SMN_THM_TCON: u32 = 0x0005_9800;
const SMN_INDEX: u32 = 0x60;
const SMN_DATA: u32 = 0x64;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Readings {
    pub cpu_temp: Option<f32>,
    pub cpu_load: Option<f32>,
    pub gpu_temp: Option<f32>,
}

pub struct Sensors {
    ring0: Option<HANDLE>,
    /// Mutex partagé avec LibreHardwareMonitor & co pour l'accès index/données SMN.
    pci_mutex: HANDLE,
    gpu: Option<Gpu>,
    last_times: Option<(u64, u64)>,
}

/// Carte graphique suivie via D3DKMT (gdi32).
struct Gpu {
    adapter: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AdapterInfo {
    handle: u32,
    luid_low: u32,
    luid_high: i32,
    sources: u32,
    precise_present: i32,
}

#[repr(C)]
struct EnumAdapters2 {
    count: u32,
    adapters: *mut AdapterInfo,
}

#[repr(C)]
struct QueryAdapterInfo {
    adapter: u32,
    kind: u32,
    data: *mut u8,
    size: u32,
}

#[repr(C)]
struct CloseAdapter {
    adapter: u32,
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn D3DKMTEnumAdapters2(e: *mut EnumAdapters2) -> i32;
    fn D3DKMTQueryAdapterInfo(q: *const QueryAdapterInfo) -> i32;
    fn D3DKMTCloseAdapter(c: *const CloseAdapter) -> i32;
}

const KMTQAITYPE_ADAPTERPERFDATA: u32 = 62;

/// Température (°C) d'un adaptateur ; `D3DKMT_ADAPTER_PERFDATA.Temperature` est en dixièmes de degré.
fn adapter_temp(adapter: u32) -> Option<f32> {
    let mut perf = [0u8; 64];
    let q = QueryAdapterInfo { adapter, kind: KMTQAITYPE_ADAPTERPERFDATA, data: perf.as_mut_ptr(), size: 64 };
    if unsafe { D3DKMTQueryAdapterInfo(&q) } != 0 {
        return None;
    }
    let deci = u32::from_le_bytes([perf[56], perf[57], perf[58], perf[59]]);
    (deci > 0).then(|| deci as f32 / 10.0)
}

impl Gpu {
    /// Garde le premier adaptateur qui rapporte une température (le GPU dédié).
    fn find() -> Option<Self> {
        let mut e = EnumAdapters2 { count: 0, adapters: null_mut() };
        if unsafe { D3DKMTEnumAdapters2(&mut e) } != 0 || e.count == 0 {
            return None;
        }
        let mut list = vec![AdapterInfo::default(); e.count as usize];
        e.adapters = list.as_mut_ptr();
        if unsafe { D3DKMTEnumAdapters2(&mut e) } != 0 {
            return None;
        }
        list.truncate(e.count as usize);
        let chosen = list.iter().map(|a| a.handle).find(|&h| adapter_temp(h).is_some());
        for a in list.iter().filter(|a| Some(a.handle) != chosen) {
            unsafe { D3DKMTCloseAdapter(&CloseAdapter { adapter: a.handle }) };
        }
        chosen.map(|adapter| Self { adapter })
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        unsafe { D3DKMTCloseAdapter(&CloseAdapter { adapter: self.adapter }) };
    }
}

impl Sensors {
    pub fn new() -> Self {
        let ring0 = unsafe {
            let h = CreateFileW(
                wide(r"\\.\WinRing0_1_2_0").as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                null_mut(),
            );
            (h != INVALID_HANDLE_VALUE).then_some(h)
        };
        let pci_mutex = unsafe { CreateMutexW(null(), 0, wide(r"Global\Access_PCI").as_ptr()) };
        Self { ring0, pci_mutex, gpu: Gpu::find(), last_times: None }
    }

    pub fn read(&mut self) -> Readings {
        let gpu_temp = self.gpu.as_ref().and_then(|g| adapter_temp(g.adapter));
        Readings { cpu_temp: self.cpu_temp(), cpu_load: self.cpu_load(), gpu_temp }
    }

    fn cpu_temp(&self) -> Option<f32> {
        let h = self.ring0?;
        let locked = !self.pci_mutex.is_null()
            && matches!(unsafe { WaitForSingleObject(self.pci_mutex, 100) }, WAIT_OBJECT_0 | WAIT_ABANDONED);
        let mut ret = 0u32;
        let write: [u32; 3] = [0, SMN_INDEX, SMN_THM_TCON];
        let read: [u32; 2] = [0, SMN_DATA];
        let mut value = 0u32;
        let ok = unsafe {
            DeviceIoControl(h, IOCTL_WRITE_PCI_CONFIG, write.as_ptr().cast(), 12, null_mut(), 0, &mut ret, null_mut()) != 0
                && DeviceIoControl(h, IOCTL_READ_PCI_CONFIG, read.as_ptr().cast(), 8, (&raw mut value).cast(), 4, &mut ret, null_mut())
                    != 0
        };
        if locked {
            unsafe { ReleaseMutex(self.pci_mutex) };
        }
        if !ok || value == 0 {
            return None;
        }
        let mut t = ((value >> 21) & 0x7FF) as f32 / 8.0;
        if value & (1 << 19) != 0 {
            t -= 49.0;
        }
        (0.0..125.0).contains(&t).then_some(t)
    }

    fn cpu_load(&mut self) -> Option<f32> {
        let ft = |f: FILETIME| (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64;
        let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut idle, mut kernel, mut user) = (zero, zero, zero);
        if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
            return None;
        }
        // kernel inclut le temps idle.
        let (idle, total) = (ft(idle), ft(kernel) + ft(user));
        let prev = self.last_times.replace((idle, total))?;
        let dt = total.saturating_sub(prev.1);
        (dt > 0).then(|| (1.0 - idle.saturating_sub(prev.0) as f32 / dt as f32).clamp(0.0, 1.0) * 100.0)
    }
}

impl Drop for Sensors {
    fn drop(&mut self) {
        unsafe {
            if let Some(h) = self.ring0 {
                CloseHandle(h);
            }
            if !self.pci_mutex.is_null() {
                CloseHandle(self.pci_mutex);
            }
        }
    }
}
