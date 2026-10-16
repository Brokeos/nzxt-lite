#![windows_subsystem = "windows"]

mod config;
mod daemon;
mod gui;
mod kraken;
mod lcd;
mod render;
mod sensors;
mod system;

use config::Config;
use kraken::Kraken;
use std::process::ExitCode;

const HELP: &str = "\
nzxt-lite — remplaçant léger de NZXT CAM pour Kraken Z

  nzxt-lite                 ouvre la fenêtre de réglages
  nzxt-lite apply           applique config.toml à la Kraken puis quitte
  nzxt-lite daemon          service en fond : icône de notification, réapplique la
                            config (modification, reconnexion, veille) et dessine
                            l'infographie CPU/GPU/liquide sur l'écran
  nzxt-lite status          températures, pompe et ventilateurs
  nzxt-lite autostart on|off  lance `daemon` à chaque ouverture de session

Les courbes tournent dans le firmware de la Kraken.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return match gui::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        };
    }
    attach_console();
    let result = match args[0].as_str() {
        "apply" => apply(),
        "daemon" => daemon::run(),
        "status" => status(),
        // Commande cachée : rend l'infographie actuelle dans un PNG (mise au point du rendu).
        "render-preview" => render_preview(args.get(1).map_or("infographie.png", String::as_str)),
        "autostart" => match args.get(1).map(String::as_str) {
            Some("on") => system::set_autostart(true),
            Some("off") => system::set_autostart(false),
            _ => Err("usage : nzxt-lite autostart on|off".into()),
        },
        _ => {
            println!("{HELP}");
            Ok(())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("erreur : {e}");
            ExitCode::FAILURE
        }
    }
}

fn apply() -> Result<(), String> {
    let cfg = Config::load()?;
    // Au démarrage de session l'USB peut ne pas être prêt : quelques tentatives.
    let mut last = String::new();
    for attempt in 0..5 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
        match Kraken::open().map_err(|e| e.to_string()).and_then(|mut k| cfg.apply(&mut k, true)) {
            Ok(()) => return Ok(()),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn status() -> Result<(), String> {
    let k = Kraken::open().map_err(|e| e.to_string())?;
    let fw = k.initialize().map_err(|e| e.to_string())?;
    let s = k.status().map_err(|e| e.to_string())?;
    println!("NZXT Kraken Z  firmware {fw}");
    println!("liquide : {:.1} °C", s.liquid_c);
    println!("pompe   : {} rpm ({}%)", s.pump_rpm, s.pump_duty);
    println!("ventilo : {} rpm ({}%)", s.fan_rpm, s.fan_duty);
    let r = sensors::Sensors::new().read();
    let fmt = |v: Option<f32>| v.map_or("n/d".to_string(), |t| format!("{t:.1} °C"));
    println!("CPU     : {}", fmt(r.cpu_temp));
    println!("GPU     : {}", fmt(r.gpu_temp));
    if let Ok(l) = k.lcd_info() {
        println!("écran   : luminosité {}%, rotation {}°", l.brightness, l.orientation as u16 * 90);
    }
    Ok(())
}

fn render_preview(out: &str) -> Result<(), String> {
    let cfg = Config::load()?;
    let r = sensors::Sensors::new().read();
    let liquid = Kraken::open().ok().and_then(|k| k.status().ok()).map(|s| s.liquid_c);
    let values = render::Values { cpu: r.cpu_temp, gpu: r.gpu_temp, liquid };
    let img = render::infographic(&render::Fonts::load(), &cfg.lcd, &values);
    img.save(out).map_err(|e| format!("{out} : {e}"))?;
    println!("{out} : GPU {:?} · CPU {:?} · liquide {:?}", values.gpu, values.cpu, values.liquid);
    Ok(())
}

/// Le binaire est en sous-système GUI : on se rattache à la console parente pour le mode CLI.
fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
