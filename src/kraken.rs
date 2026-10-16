//! Protocole NZXT Kraken Z (Z53/Z63/Z73, USB 1E71:3008).
//!
//! Référence : pilote `kraken3.py` de liquidctl. Commandes HID de 64 octets sur
//! l'interface vendeur ; l'écran reçoit ses images en bulk (WinUSB, endpoint 0x02).

use hidapi::{HidApi, HidDevice};
use std::{fmt, time::Duration};

pub const VENDOR_ID: u16 = 0x1E71;
pub const PRODUCT_ID: u16 = 0x3008;
const REPORT_LEN: usize = 64;
const MAX_READS: usize = 12;
const READ_TIMEOUT_MS: i32 = 500;
pub const CURVE_MIN_TEMP: u8 = 20;
pub const CRITICAL_TEMP: u8 = 59;
pub const LCD_SIZE: u32 = 320;
const BULK_ENDPOINT: u8 = 0x02;
const BULK_CHUNK: usize = 512;
/// Mémoire écran, en blocs de 1 Kio.
const LCD_MEMORY_KIB: usize = 24_320;
const BULK_MAGIC: [u8; 12] = [0x12, 0xFA, 0x01, 0xE8, 0xAB, 0xCD, 0xEF, 0x98, 0x76, 0x54, 0x32, 0x10];

#[derive(Debug)]
pub enum Error {
    NotFound,
    Busy,
    Hid(hidapi::HidError),
    Usb(rusb::Error),
    NoReply(u8),
    Device(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotFound => write!(f, "Kraken Z introuvable"),
            Error::Busy => write!(f, "Kraken occupée : ferme NZXT CAM (et liquidctl/SignalRGB)"),
            Error::Hid(e) => write!(f, "erreur HID : {e}"),
            Error::Usb(e) => write!(f, "erreur USB (écran) : {e}"),
            Error::NoReply(c) => write!(f, "pas de réponse à la commande 0x{c:02X}"),
            Error::Device(m) => write!(f, "{m}"),
        }
    }
}

impl From<hidapi::HidError> for Error {
    fn from(e: hidapi::HidError) -> Self {
        match &e {
            hidapi::HidError::IoError { error } if error.kind() == std::io::ErrorKind::PermissionDenied => Error::Busy,
            _ => Error::Hid(e),
        }
    }
}

impl From<rusb::Error> for Error {
    fn from(e: rusb::Error) -> Self {
        match e {
            rusb::Error::Access | rusb::Error::Busy => Error::Busy,
            _ => Error::Usb(e),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Pump,
    Fan,
}

impl Channel {
    fn id(self) -> u8 {
        match self {
            Channel::Pump => 0x01,
            Channel::Fan => 0x02,
        }
    }

    /// Duty minimum accepté (la pompe ne doit jamais s'arrêter).
    pub fn min_duty(self) -> u8 {
        match self {
            Channel::Pump => 20,
            Channel::Fan => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Status {
    pub liquid_c: f32,
    pub pump_rpm: u16,
    pub pump_duty: u8,
    pub fan_rpm: u16,
    pub fan_duty: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct LcdInfo {
    pub brightness: u8,
    /// 0 = 0°, 1 = 90°, 2 = 180°, 3 = 270°.
    pub orientation: u8,
}

/// Données envoyées à l'écran.
pub enum Picture {
    /// RGBA 320×320 (le 4e octet est ignoré par l'appareil).
    Static(Vec<u8>),
    /// Fichier GIF déjà redimensionné.
    Gif(Vec<u8>),
}

pub struct Kraken {
    dev: HidDevice,
    serial: Option<String>,
    /// Interface bulk ouverte pour le flux d'images (infographie) + prochain bucket.
    stream: Option<(rusb::DeviceHandle<rusb::GlobalContext>, u8)>,
}

impl Kraken {
    pub fn open() -> Result<Self> {
        let api = HidApi::new()?;
        let entry = api
            .device_list()
            .find(|d| d.vendor_id() == VENDOR_ID && d.product_id() == PRODUCT_ID)
            .ok_or(Error::NotFound)?;
        let serial = entry.serial_number().map(str::to_owned);
        let dev = entry.open_device(&api)?;
        Ok(Self { dev, serial, stream: None })
    }

    fn write(&self, data: &[u8]) -> Result<()> {
        let mut buf = [0u8; REPORT_LEN];
        buf[..data.len()].copy_from_slice(data);
        self.dev.write(&buf)?;
        Ok(())
    }

    /// Lit jusqu'à recevoir le message `[cmd + 1, sub, ...]`.
    fn read_reply(&self, cmd: u8, sub: u8) -> Result<[u8; REPORT_LEN]> {
        let mut buf = [0u8; REPORT_LEN];
        for _ in 0..MAX_READS {
            let n = self.dev.read_timeout(&mut buf, READ_TIMEOUT_MS)?;
            if n >= 2 && buf[0] == cmd + 1 && buf[1] == sub {
                return Ok(buf);
            }
            if n == 0 {
                break;
            }
        }
        Err(Error::NoReply(cmd))
    }

    fn request(&self, data: &[u8]) -> Result<[u8; REPORT_LEN]> {
        self.drain();
        self.write(data)?;
        self.read_reply(data[0], data[1])
    }

    fn drain(&self) {
        let mut buf = [0u8; REPORT_LEN];
        while matches!(self.dev.read_timeout(&mut buf, 0), Ok(n) if n > 0) {}
    }

    /// À faire après chaque démarrage / reconnexion.
    pub fn initialize(&self) -> Result<String> {
        // Intervalle de mise à jour 0,5 s (cf. liquidctl #128).
        self.write(&[0x70, 0x02, 0x01, 0xB8, 0x01])?;
        self.write(&[0x70, 0x01])?;
        let fw = self.request(&[0x10, 0x01])?;
        Ok(format!("{}.{}.{}", fw[0x11], fw[0x12], fw[0x13]))
    }

    pub fn status(&self) -> Result<Status> {
        let m = self.request(&[0x74, 0x01])?;
        if m[15] == 0xFF && m[16] == 0xFF {
            return Err(Error::Device("température du liquide invalide (firmware)"));
        }
        Ok(Status {
            liquid_c: m[15] as f32 + m[16] as f32 / 10.0,
            pump_rpm: u16::from_le_bytes([m[17], m[18]]),
            pump_duty: m[19],
            fan_rpm: u16::from_le_bytes([m[23], m[24]]),
            fan_duty: m[25],
        })
    }

    /// `duties[i]` = duty (%) pour une température liquide de `20 + i` °C.
    pub fn set_curve(&self, channel: Channel, duties: &[u8; 40]) -> Result<()> {
        let mut msg = [0u8; 44];
        msg[..4].copy_from_slice(&[0x72, channel.id(), 0x00, 0x00]);
        for (dst, &d) in msg[4..].iter_mut().zip(duties) {
            *dst = d.clamp(channel.min_duty(), 100);
        }
        self.write(&msg)
    }

    pub fn lcd_info(&self) -> Result<LcdInfo> {
        let m = self.request(&[0x30, 0x01])?;
        Ok(LcdInfo { brightness: m[0x18], orientation: m[0x1A] })
    }

    pub fn set_lcd(&self, brightness: u8, orientation: u8) -> Result<()> {
        self.write(&[0x30, 0x02, 0x01, brightness.min(100), 0x00, 0x00, 0x01, orientation % 4])
    }

    /// Écran intégré : température du liquide.
    pub fn show_liquid(&self) -> Result<()> {
        self.switch_bucket(0, 0x02)
    }

    fn switch_bucket(&self, bucket: u8, mode: u8) -> Result<()> {
        let r = self.request(&[0x38, 0x01, mode, bucket])?;
        if r[14] == 0x01 { Ok(()) } else { Err(Error::Device("changement de bucket écran refusé")) }
    }

    fn delete_bucket(&self, bucket: u8) -> Result<bool> {
        Ok(self.request(&[0x32, 0x02, bucket])?[14] == 0x01)
    }

    /// Envoie une image ou un GIF dans un bucket mémoire de l'écran puis l'affiche.
    pub fn upload(&mut self, picture: &Picture) -> Result<()> {
        self.stream = None;
        let usb = self.open_bulk()?;
        self.reset_buckets()?;
        self.write_bucket(&usb, 0, 0, picture)
    }

    /// Affiche une image RGBA 320×320 en alternant deux buckets (pas de scintillement).
    /// Pensé pour un rafraîchissement continu : l'interface bulk reste ouverte.
    pub fn push_frame(&mut self, rgba: Vec<u8>) -> Result<()> {
        if self.stream.is_none() {
            let usb = self.open_bulk()?;
            self.reset_buckets()?;
            self.stream = Some((usb, 0));
        }
        let picture = Picture::Static(rgba);
        let blocks = Self::blocks(&picture) as u16;
        let (usb, bucket) = self.stream.take().expect("stream ouvert");
        let result = self.write_bucket(&usb, bucket, bucket as u16 * blocks, &picture);
        if result.is_ok() {
            self.stream = Some((usb, bucket ^ 1));
        }
        result
    }

    fn blocks(picture: &Picture) -> usize {
        let len = match picture {
            Picture::Static(d) | Picture::Gif(d) => d.len(),
        };
        (BULK_MAGIC.len() + 8 + len).div_ceil(1024)
    }

    /// Repart d'une mémoire écran propre (évite la gestion de fragmentation des buckets).
    fn reset_buckets(&self) -> Result<()> {
        self.request(&[0x36, 0x03])?;
        self.show_liquid()?;
        for b in 0..16 {
            self.delete_bucket(b)?;
        }
        Ok(())
    }

    fn write_bucket(&self, usb: &rusb::DeviceHandle<rusb::GlobalContext>, bucket: u8, offset_kib: u16, picture: &Picture) -> Result<()> {
        let (kind, data) = match picture {
            Picture::Static(d) => (0x02u8, d.as_slice()),
            Picture::Gif(d) => (0x01u8, d.as_slice()),
        };
        let blocks = Self::blocks(picture);
        if offset_kib as usize + blocks >= LCD_MEMORY_KIB {
            return Err(Error::Device("fichier trop gros pour la mémoire de l'écran (24 Mo)"));
        }
        self.delete_bucket(bucket)?;
        let (off, size) = (offset_kib.to_le_bytes(), (blocks as u16).to_le_bytes());
        let setup = self.request(&[0x32, 0x01, bucket, bucket + 1, off[0], off[1], size[0], size[1], 0x01])?;
        if setup[14] != 0x01 {
            return Err(Error::Device("préparation du bucket écran refusée"));
        }
        self.request(&[0x36, 0x01, bucket])?;
        let mut header = BULK_MAGIC.to_vec();
        header.extend([kind, 0, 0, 0]);
        header.extend((data.len() as u32).to_le_bytes());
        let timeout = Duration::from_secs(2);
        usb.write_bulk(BULK_ENDPOINT, &header, timeout)?;
        for chunk in data.chunks(BULK_CHUNK) {
            usb.write_bulk(BULK_ENDPOINT, chunk, timeout)?;
        }
        self.write(&[0x36, 0x02])?;
        self.switch_bucket(bucket, 0x04)
    }

    fn open_bulk(&self) -> Result<rusb::DeviceHandle<rusb::GlobalContext>> {
        for device in rusb::devices()?.iter() {
            let Ok(desc) = device.device_descriptor() else { continue };
            if desc.vendor_id() != VENDOR_ID || desc.product_id() != PRODUCT_ID {
                continue;
            }
            let handle = device.open()?;
            let same_serial = match (&self.serial, desc.serial_number_string_index()) {
                (Some(s), Some(_)) => handle.read_serial_number_string_ascii(&desc).is_ok_and(|v| &v == s),
                _ => true,
            };
            if same_serial {
                handle.claim_interface(0)?;
                return Ok(handle);
            }
        }
        Err(Error::Device("interface écran (WinUSB) introuvable"))
    }
}

/// Interpole une courbe (temp °C, duty %) sur 20..=59 °C, comme liquidctl :
/// points triés, duty forcé à 100 % à la température critique.
pub fn interpolate(points: &[(u8, u8)], channel: Channel) -> [u8; 40] {
    let mut pts: Vec<(u8, u8)> = points.iter().copied().filter(|p| p.0 < CRITICAL_TEMP).collect();
    pts.sort_by_key(|p| p.0);
    pts.dedup_by_key(|p| p.0);
    pts.push((CRITICAL_TEMP, 100));
    std::array::from_fn(|i| {
        let t = CURVE_MIN_TEMP + i as u8;
        let duty = match pts.iter().position(|p| p.0 >= t) {
            Some(0) => pts[0].1 as f32,
            Some(j) => {
                let (a, b) = (pts[j - 1], pts[j]);
                let f = (t - a.0) as f32 / (b.0 - a.0) as f32;
                a.1 as f32 + f * (b.1 as f32 - a.1 as f32)
            }
            None => 100.0,
        };
        (duty.round() as u8).clamp(channel.min_duty(), 100)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_like_liquidctl() {
        let d = interpolate(&[(20, 30), (40, 50)], Channel::Fan);
        assert_eq!(d[0], 30);
        assert_eq!(d[10], 40);
        assert_eq!(d[20], 50);
        // de 40 °C (50 %) à 59 °C (100 %)
        assert_eq!(d[39], 100);
        assert!(d.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn pump_never_below_minimum() {
        let d = interpolate(&[(20, 0), (30, 0)], Channel::Pump);
        assert!(d.iter().all(|&v| v >= 20));
    }

    #[test]
    fn fixed_profile_is_flat_until_critical() {
        let d = interpolate(&[(20, 60), (58, 60)], Channel::Fan);
        assert!(d[..39].iter().all(|&v| v == 60));
        assert_eq!(d[39], 100);
    }
}
