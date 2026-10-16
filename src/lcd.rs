//! Préparation des images pour l'écran 320×320 de la Kraken Z.

use crate::kraken::{LCD_SIZE, Picture};
use image::codecs::gif::{GifDecoder, GifEncoder, Repeat};
use image::imageops::{self, FilterType};
use image::{AnimationDecoder, DynamicImage, Frame, RgbaImage};
use std::io::BufReader;
use std::path::Path;

fn fit(img: &RgbaImage, orientation: u8) -> RgbaImage {
    let resized = imageops::resize(img, LCD_SIZE, LCD_SIZE, FilterType::Triangle);
    match orientation % 4 {
        1 => imageops::rotate90(&resized),
        2 => imageops::rotate180(&resized),
        3 => imageops::rotate270(&resized),
        _ => resized,
    }
}

fn is_gif(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("gif"))
}

pub fn prepare(path: &Path, orientation: u8) -> Result<Picture, String> {
    let err = |e: &dyn std::fmt::Display| format!("{} : {e}", path.display());
    if is_gif(path) {
        let file = std::fs::File::open(path).map_err(|e| err(&e))?;
        let frames = GifDecoder::new(BufReader::new(file)).map_err(|e| err(&e))?.into_frames();
        let mut out = Vec::new();
        {
            let mut enc = GifEncoder::new_with_speed(&mut out, 10);
            enc.set_repeat(Repeat::Infinite).map_err(|e| err(&e))?;
            for frame in frames {
                let frame = frame.map_err(|e| err(&e))?;
                let delay = frame.delay();
                let fitted = fit(frame.buffer(), orientation);
                enc.encode_frame(Frame::from_parts(fitted, 0, 0, delay)).map_err(|e| err(&e))?;
            }
        }
        return Ok(Picture::Gif(out));
    }
    let img: DynamicImage = image::open(path).map_err(|e| err(&e))?;
    let mut rgba = fit(&img.to_rgba8(), orientation).into_raw();
    // Format attendu : R, G, B, 0.
    rgba.chunks_exact_mut(4).for_each(|px| px[3] = 0);
    Ok(Picture::Static(rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_image_is_320_square_rgb0() {
        let dir = std::env::temp_dir().join("nzxt-lite-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("red.png");
        RgbaImage::from_pixel(64, 32, image::Rgba([255, 0, 0, 255])).save(&p).unwrap();
        let Picture::Static(data) = prepare(&p, 1).unwrap() else { panic!("expected static") };
        assert_eq!(data.len(), (LCD_SIZE * LCD_SIZE * 4) as usize);
        assert_eq!(&data[..4], &[255, 0, 0, 0]);
    }
}
