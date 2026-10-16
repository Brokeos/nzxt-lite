//! Rendu de l'infographie triple (façon NZXT CAM) pour l'écran rond 320×320.

use crate::config::{Lcd, Reading};
use crate::kraken::LCD_SIZE;
use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use std::sync::Mutex;
use image::RgbaImage;
use tiny_skia::{Color, Paint, PathBuilder, Pixmap, Stroke, Transform};

/// Police système (C:\Windows\Fonts), projetée en mémoire une seule fois par processus.
pub fn system_font(file: &'static str) -> Option<&'static [u8]> {
    static CACHE: Mutex<Vec<(&str, &[u8])>> = Mutex::new(Vec::new());
    let mut cache = CACHE.lock().ok()?;
    if let Some(&(_, data)) = cache.iter().find(|(f, _)| *f == file) {
        return Some(data);
    }
    let data = crate::system::map_file(&format!(r"C:\Windows\Fonts\{file}"))?;
    cache.push((file, data));
    Some(data)
}

pub struct Fonts {
    bold: FontRef<'static>,
    semibold: FontRef<'static>,
}

impl Fonts {
    /// Segoe UI (Windows) si disponible, sinon la police embarquée d'egui.
    pub fn load() -> Self {
        let load = |file| {
            system_font(file)
                .and_then(|d| FontRef::try_from_slice(d).ok())
                .unwrap_or_else(|| FontRef::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT).expect("police embarquée"))
        };
        Self { bold: load("segoeuib.ttf"), semibold: load("seguisb.ttf") }
    }
}

/// Valeurs à afficher, en °C.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Values {
    pub cpu: Option<f32>,
    pub gpu: Option<f32>,
    pub liquid: Option<f32>,
}

impl Values {
    fn get(&self, r: Reading) -> Option<f32> {
        match r {
            Reading::Cpu => self.cpu,
            Reading::Gpu => self.gpu,
            Reading::Liquid => self.liquid,
        }
    }

    /// Clé de rafraîchissement : on ne renvoie une image que si un entier affiché change.
    pub fn key(&self) -> [Option<i32>; 3] {
        [self.cpu, self.gpu, self.liquid].map(|v| v.map(|t| t.round() as i32))
    }
}

/// Largeur de `text`, avec `tracking` pixels ajoutés entre les lettres.
fn text_width(font: &FontRef<'static>, size: f32, text: &str, tracking: f32) -> f32 {
    let f = font.as_scaled(PxScale::from(size));
    let mut w = 0.0;
    let mut prev = None;
    for c in text.chars() {
        let id = f.glyph_id(c);
        if let Some(p) = prev {
            w += f.kern(p, id) + tracking;
        }
        w += f.h_advance(id);
        prev = Some(id);
    }
    w
}

/// Dessine `text` à partir de `x`, ligne de base `baseline`.
fn draw_text(pm: &mut Pixmap, font: &FontRef<'static>, size: f32, text: &str, mut x: f32, baseline: f32, tracking: f32, rgb: [u8; 3]) {
    let scaled = font.as_scaled(PxScale::from(size));
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    let data = pm.data_mut();
    let mut prev = None;
    for c in text.chars() {
        let id = scaled.glyph_id(c);
        if let Some(p) = prev {
            x += scaled.kern(p, id) + tracking;
        }
        let glyph = id.with_scale_and_position(PxScale::from(size), ab_glyph::point(x, baseline));
        if let Some(outline) = font.outline_glyph(glyph) {
            let b = outline.px_bounds();
            outline.draw(|gx, gy, cov| {
                let (px, py) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
                if px < 0 || py < 0 || px >= w || py >= h {
                    return;
                }
                let i = (py * w + px) as usize * 4;
                let a = cov.clamp(0.0, 1.0);
                for k in 0..3 {
                    data[i + k] = (data[i + k] as f32 * (1.0 - a) + rgb[k] as f32 * a) as u8;
                }
                data[i + 3] = 255;
            });
        }
        x += scaled.h_advance(id);
        prev = Some(id);
    }
}

fn draw_centered(pm: &mut Pixmap, font: &FontRef<'static>, size: f32, text: &str, cx: f32, baseline: f32, tracking: f32, rgb: [u8; 3]) {
    let x = cx - text_width(font, size, text, tracking) / 2.0;
    draw_text(pm, font, size, text, x, baseline, tracking, rgb);
}

/// Température « 48° » : les chiffres sont centrés sur `cx`, le degré (plus petit) accroché à droite.
fn draw_temp(pm: &mut Pixmap, fonts: &Fonts, size: f32, value: Option<f32>, cx: f32, baseline: f32) {
    let Some(t) = value else {
        draw_centered(pm, &fonts.bold, size, "--", cx, baseline, 0.0, DIM);
        return;
    };
    let digits = format!("{:.0}", t);
    let w = text_width(&fonts.bold, size, &digits, 0.0);
    let deg = size * 0.55;
    // Léger décalage à gauche pour que l'ensemble « 48° » paraisse centré.
    let x = cx - w / 2.0 - deg * 0.18;
    draw_text(pm, &fonts.bold, size, &digits, x, baseline, 0.0, WHITE);
    draw_text(pm, &fonts.semibold, deg, "°", x + w + 2.0, baseline - size * 0.34, 0.0, WHITE);
}

const WHITE: [u8; 3] = [255, 255, 255];
const DIM: [u8; 3] = [165, 167, 178];

fn stroke_line(pm: &mut Pixmap, paint: &Paint, (x0, y0): (f32, f32), (x1, y1): (f32, f32)) {
    let mut pb = PathBuilder::new();
    pb.move_to(x0, y0);
    pb.line_to(x1, y1);
    if let Some(p) = pb.finish() {
        let stroke = Stroke { width: 2.0, line_cap: tiny_skia::LineCap::Round, ..Default::default() };
        pm.stroke_path(&p, paint, &stroke, Transform::identity(), None);
    }
}

/// Image RGBA 320×320 (non tournée), dans l'esprit de « TripleInfographic » de NZXT CAM :
/// deux valeurs en haut, une en bas, logo et anneau blanc au bord de l'écran rond.
pub fn infographic(fonts: &Fonts, lcd: &Lcd, values: &Values) -> RgbaImage {
    let size = LCD_SIZE as f32;
    let c = size / 2.0;
    let mut pm = Pixmap::new(LCD_SIZE, LCD_SIZE).expect("pixmap");
    pm.fill(Color::BLACK);

    let mut paint = Paint::default();
    paint.anti_alias = true;
    paint.set_color_rgba8(255, 255, 255, 255);
    // Anneau légèrement en retrait : le bord physique de la dalle rogne les derniers pixels.
    if let Some(ring) = PathBuilder::from_circle(c, c, c - 7.0) {
        pm.stroke_path(&ring, &paint, &Stroke { width: 4.0, ..Default::default() }, Transform::identity(), None);
    }
    paint.set_color_rgba8(255, 255, 255, 56);
    stroke_line(&mut pm, &paint, (c, 112.0), (c, 186.0));
    stroke_line(&mut pm, &paint, (78.0, 203.0), (size - 78.0, 203.0));

    if lcd.logo {
        draw_centered(&mut pm, &fonts.bold, 30.0, "NZXT", c, 76.0, 3.0, WHITE);
    }
    let [left, right, bottom] = lcd.readings;
    for (r, cx) in [(left, 98.0), (right, size - 98.0)] {
        draw_centered(&mut pm, &fonts.semibold, 17.0, r.label(), cx, 120.0, 2.0, DIM);
        draw_temp(&mut pm, fonts, 78.0, values.get(r), cx, 182.0);
    }
    draw_centered(&mut pm, &fonts.semibold, 17.0, bottom.label(), c, 232.0, 2.0, DIM);
    draw_temp(&mut pm, fonts, 70.0, values.get(bottom), c, 286.0);

    RgbaImage::from_raw(LCD_SIZE, LCD_SIZE, pm.take()).expect("taille image")
}

/// Prépare une image pour l'appareil : rotation selon l'orientation, format R, G, B, 0.
pub fn to_device(img: &RgbaImage, orientation: u8) -> Vec<u8> {
    let rotated = match orientation % 4 {
        1 => image::imageops::rotate90(img),
        2 => image::imageops::rotate180(img),
        3 => image::imageops::rotate270(img),
        _ => img.clone(),
    };
    let mut raw = rotated.into_raw();
    raw.chunks_exact_mut(4).for_each(|px| px[3] = 0);
    raw
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn infographic_is_round_screen_sized() {
        let values = Values { cpu: Some(65.4), gpu: Some(48.0), liquid: Some(37.2) };
        let img = infographic(&Fonts::load(), &Config::default().lcd, &values);
        assert_eq!(img.dimensions(), (LCD_SIZE, LCD_SIZE));
        // Coin noir (hors de la dalle ronde), anneau blanc sur le bord gauche.
        assert_eq!(img.get_pixel(0, 0).0[..3], [0, 0, 0]);
        assert!(img.get_pixel(7, LCD_SIZE / 2).0[0] > 200);
        assert_eq!(values.key(), [Some(65), Some(48), Some(37)]);
    }
}
