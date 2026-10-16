use crate::kraken::{self, Channel, Kraken, Picture};
use crate::lcd;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Curve,
    Fixed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub mode: Mode,
    /// Duty (%) en mode fixe.
    pub fixed: u8,
    /// Points (température liquide °C, duty %).
    pub curve: Vec<(u8, u8)>,
}

impl Profile {
    pub fn duties(&self, channel: Channel) -> [u8; 40] {
        match self.mode {
            Mode::Fixed => kraken::interpolate(&[(20, self.fixed), (kraken::CRITICAL_TEMP - 1, self.fixed)], channel),
            Mode::Curve => kraken::interpolate(&self.curve, channel),
        }
    }

    /// Préréglage « Silent » de NZXT CAM (courbes sur la température du liquide).
    pub fn silent(channel: Channel) -> Self {
        let curve = match channel {
            Channel::Pump => vec![(20, 60), (35, 60), (55, 100)],
            Channel::Fan => vec![(20, 30), (35, 30), (40, 35), (45, 42), (50, 50), (55, 57)],
        };
        Self { mode: Mode::Curve, fixed: 60, curve }
    }

    /// Préréglage « Performance » de NZXT CAM.
    pub fn performance(channel: Channel) -> Self {
        let curve = match channel {
            Channel::Pump => vec![(20, 70), (35, 70), (40, 80), (55, 95)],
            Channel::Fan => vec![(20, 50), (35, 50), (55, 90)],
        };
        let fixed = match channel {
            Channel::Pump => 100,
            Channel::Fan => 40,
        };
        Self { mode: Mode::Curve, fixed, curve }
    }
}

/// Valeur affichée dans un emplacement de l'infographie.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reading {
    Cpu,
    Gpu,
    Liquid,
}

impl Reading {
    pub const ALL: [Reading; 3] = [Reading::Cpu, Reading::Gpu, Reading::Liquid];

    pub fn label(self) -> &'static str {
        match self {
            Reading::Cpu => "CPU",
            Reading::Gpu => "GPU",
            Reading::Liquid => "LIQUIDE",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LcdMode {
    /// Infographie triple dessinée par nzxt-lite (nécessite le service résident).
    Infographic,
    /// Affichage intégré de la température du liquide.
    Liquid,
    /// Image fixe ou GIF animé (envoyé une fois, joué par l'écran lui-même).
    Image,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lcd {
    pub mode: LcdMode,
    pub brightness: u8,
    /// 0, 90, 180 ou 270.
    pub rotation: u16,
    pub image: Option<PathBuf>,
    /// Emplacements de l'infographie : gauche, droite, bas.
    #[serde(default = "default_readings")]
    pub readings: [Reading; 3],
    #[serde(default = "yes")]
    pub logo: bool,
}

fn default_readings() -> [Reading; 3] {
    [Reading::Gpu, Reading::Cpu, Reading::Liquid]
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub pump: Profile,
    pub fan: Profile,
    pub lcd: Lcd,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            pump: Profile::performance(Channel::Pump),
            fan: Profile::performance(Channel::Fan),
            lcd: Lcd {
                mode: LcdMode::Infographic,
                brightness: 50,
                rotation: 0,
                image: None,
                readings: default_readings(),
                logo: true,
            },
        }
    }
}

pub fn path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("nzxt-lite").join("config.toml")
}

impl Config {
    pub fn load() -> Result<Self, String> {
        let p = path();
        match std::fs::read_to_string(&p) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{} : {e}", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{} : {e}", p.display())),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let p = path();
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        // Écriture atomique : on ne laisse jamais un fichier tronqué.
        let tmp = p.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
    }

    /// Envoie toute la configuration à la Kraken. `upload` contrôle l'envoi de l'image écran.
    pub fn apply(&self, k: &mut Kraken, upload: bool) -> Result<(), String> {
        let e = |e: kraken::Error| e.to_string();
        k.initialize().map_err(e)?;
        k.set_curve(Channel::Pump, &self.pump.duties(Channel::Pump)).map_err(e)?;
        k.set_curve(Channel::Fan, &self.fan.duties(Channel::Fan)).map_err(e)?;
        let orientation = (self.lcd.rotation / 90 % 4) as u8;
        k.set_lcd(self.lcd.brightness, orientation).map_err(e)?;
        match (&self.lcd.mode, &self.lcd.image) {
            (LcdMode::Image, Some(path)) if upload => {
                let picture: Picture = lcd::prepare(path, orientation)?;
                k.upload(&picture).map_err(e)
            }
            (LcdMode::Image, _) if upload => Err("aucune image choisie pour l'écran".into()),
            (LcdMode::Image, _) => Ok(()),
            (LcdMode::Liquid, _) => k.show_liquid().map_err(e),
            // L'infographie est dessinée en continu par le service (`daemon`).
            (LcdMode::Infographic, _) => Ok(()),
        }
    }
}
