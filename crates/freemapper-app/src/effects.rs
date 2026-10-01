//! MadMapper風デフォルトエフェクト (v0.1: CPU処理)。
//! 将来はGPUシェーダ化するが、パラメータUIと適用順はこのまま引き継ぐ。

use image::RgbaImage;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Effects {
    pub brightness: i32, // -100..100
    pub contrast: f32,   // -100..100 (%)
    pub saturation: f32, // 0..200 (%)
    pub hue_shift: i32,  // -180..180 (deg)
    pub grayscale: bool,
    pub invert: bool,
    pub blur_sigma: f32, // 0..10
}

impl Default for Effects {
    fn default() -> Self {
        Self {
            brightness: 0,
            contrast: 0.0,
            saturation: 100.0,
            hue_shift: 0,
            grayscale: false,
            invert: false,
            blur_sigma: 0.0,
        }
    }
}

impl Effects {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn is_default(&self) -> bool {
        self.brightness == 0
            && self.contrast == 0.0
            && self.saturation == 100.0
            && self.hue_shift == 0
            && !self.grayscale
            && !self.invert
            && self.blur_sigma == 0.0
    }

    /// baseにエフェクトを適用したコピーを返す。重い順(blur)を先にやる。
    pub fn apply(&self, base: &RgbaImage) -> RgbaImage {
        if self.is_default() {
            return base.clone();
        }
        let (w, h) = base.dimensions();
        // 高解像度+blurの爆発を避ける: 1600px超は処理用に縮小 (表示用なのでOK)
        let work: RgbaImage = if w.max(h) > 1600 {
            image::imageops::resize(base, w.min(1600), h.min(1600), image::imageops::Triangle)
        } else {
            base.clone()
        };
        let mut img = if self.blur_sigma > 0.0 {
            image::imageops::blur(&work, self.blur_sigma)
        } else {
            work
        };
        if self.grayscale {
            // grayscale()はGrayImageを返すのでRGBAに戻す
            let gray = image::imageops::grayscale(&img);
            img = image::ImageBuffer::from_fn(gray.width(), gray.height(), |x, y| {
                let l = gray.get_pixel(x, y).0[0];
                image::Rgba([l, l, l, 255])
            });
        }
        if self.invert {
            image::imageops::invert(&mut img);
        }
        if self.hue_shift != 0 {
            img = image::imageops::huerotate(&img, self.hue_shift);
        }
        if self.brightness != 0 {
            img = image::imageops::brighten(&img, self.brightness);
        }
        if self.contrast != 0.0 {
            img = image::imageops::contrast(&img, self.contrast);
        }
        if self.saturation != 100.0 {
            apply_saturation(&mut img, self.saturation / 100.0);
        }
        img
    }
}

/// 彩度: 各ピクセルをlumaとの線形補間で寄せる/離す。
fn apply_saturation(img: &mut RgbaImage, s: f32) {
    for px in img.pixels_mut() {
        let [r, g, b, a] = px.0;
        let luma = (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32).round();
        let f = |c: u8| (luma + (c as f32 - luma) * s).clamp(0.0, 255.0) as u8;
        *px = image::Rgba([f(r), f(g), f(b), a]);
    }
}

/// RgbaImage -> egui表示用。
pub fn to_color_image(img: &RgbaImage) -> eframe::egui::ColorImage {
    let (w, h) = img.dimensions();
    eframe::egui::ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        &img.clone().into_raw(),
    )
}
