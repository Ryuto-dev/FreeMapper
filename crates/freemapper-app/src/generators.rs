//! ビルトイン・ジェネレーター素材 (v0.1)。
//! MadMapperのMaterial的な位置づけ: 起動直後からテスト出力できる動く素材。
//! v0.1はCPUレンダリング (480x270)。将来はGLSL/ISFシェーダ化する。

use image::RgbaImage;
use serde::{Deserialize, Serialize};

pub const GEN_W: u32 = 480;
pub const GEN_H: u32 = 270;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GeneratorKind {
    TestCard,
    Waves,
    Plasma,
    Noise,
    Lines,
    Bars,
}

impl GeneratorKind {
    pub fn all() -> [Self; 6] {
        [
            Self::TestCard,
            Self::Waves,
            Self::Plasma,
            Self::Noise,
            Self::Lines,
            Self::Bars,
        ]
    }

    /// locales/*.json のキー
    pub fn label_key(&self) -> &'static str {
        match self {
            Self::TestCard => "gen.testcard",
            Self::Waves => "gen.waves",
            Self::Plasma => "gen.plasma",
            Self::Noise => "gen.noise",
            Self::Lines => "gen.lines",
            Self::Bars => "gen.bars",
        }
    }

    pub fn uri(&self) -> &'static str {
        match self {
            Self::TestCard => "generator://test-card",
            Self::Waves => "generator://waves",
            Self::Plasma => "generator://plasma",
            Self::Noise => "generator://noise",
            Self::Lines => "generator://lines",
            Self::Bars => "generator://bars",
        }
    }

    pub fn render(&self, t: f32) -> RgbaImage {
        match self {
            Self::TestCard => render_testcard(t),
            Self::Waves => render_waves(t),
            Self::Plasma => render_plasma(t),
            Self::Noise => render_noise(t),
            Self::Lines => render_lines(t),
            Self::Bars => render_bars(t),
        }
    }
}

fn render_testcard(t: f32) -> RgbaImage {
    let pulse = (t * 2.0).sin() * 0.5 + 0.5 * 0.3 + 0.7;
    RgbaImage::from_fn(GEN_W, GEN_H, |x, y| {
        let fx = x as f32 / GEN_W as f32;
        let fy = y as f32 / GEN_H as f32;
        // グラデーション + グリッド + 動くバー (同期確認用)
        let grid = if (x % 48 == 0) || (y % 48 == 0) {
            40
        } else {
            0
        };
        let bar = if ((t * 120.0) as u32 % (GEN_W + 60)).saturating_sub(x) < 8 {
            120
        } else {
            0
        };
        let r = (fx * 255.0 * pulse) as u8;
        let g = (fy * 255.0 * pulse) as u8;
        let b = ((1.0 - fx) * 200.0) as u8;
        image::Rgba([
            r.saturating_add(grid).saturating_add(bar),
            g.saturating_add(grid),
            b.saturating_add(grid),
            255,
        ])
    })
}

/// 波: 干渉縞 (MadMapperのWaves的)
fn render_waves(t: f32) -> RgbaImage {
    RgbaImage::from_fn(GEN_W, GEN_H, |x, y| {
        let fx = x as f32 / GEN_W as f32 * 4.0;
        let fy = y as f32 / GEN_H as f32 * 4.0;
        let v =
            (fx * 3.0 + t * 2.0).sin() + (fy * 4.0 - t * 1.6).sin() + ((fx + fy) * 2.0 + t).sin();
        // -3..3 -> 0..1
        let n = (v / 3.0 * 0.5 + 0.5).clamp(0.0, 1.0);
        let hue = (n * 1.2 + t * 0.05) % 1.0;
        hsv(hue, 0.9, 0.25 + n * 0.75)
    })
}

/// プラズマ: 古典的sinプラズマ
fn render_plasma(t: f32) -> RgbaImage {
    RgbaImage::from_fn(GEN_W, GEN_H, |x, y| {
        let fx = x as f32 / GEN_W as f32;
        let fy = y as f32 / GEN_H as f32;
        let v = (fx * 10.0 + t).sin()
            + (fy * 10.0 + t * 1.3).sin()
            + ((fx + fy) * 8.0 + t * 0.7).sin()
            + (((fx - 0.5).powi(2) + (fy - 0.5).powi(2)).sqrt() * 20.0 - t * 2.0).sin();
        let n = (v / 4.0 * 0.5 + 0.5).clamp(0.0, 1.0);
        hsv((n + t * 0.03) % 1.0, 0.85, 0.2 + n * 0.8)
    })
}

/// ノイズ: 低解像度ブロックを最近傍拡大した動的ノイズ (負荷軽)
fn render_noise(t: f32) -> RgbaImage {
    const BW: u32 = 120;
    const BH: u32 = 68;
    let frame = (t * 12.0) as u64; // 12fpsでパラパラ
    let small: Vec<u8> = (0..BW * BH)
        .map(|i| hash01(i as u64 ^ frame.wrapping_mul(0x9E3779B97F4A7C15)) as u8)
        .collect();
    RgbaImage::from_fn(GEN_W, GEN_H, |x, y| {
        let sx = (x * BW / GEN_W) as usize;
        let sy = (y * BH / GEN_H) as usize;
        let v = small[sy * BW as usize + sx];
        // 白黒 + たまに色ドット
        if hash01(i64_to_u64(x, y) ^ frame) > 0.985 {
            image::Rgba([255, 60, 60, 255])
        } else {
            image::Rgba([v, v, v, 255])
        }
    })
}

fn i64_to_u64(x: u32, y: u32) -> u64 {
    ((x as u64) << 32) | y as u64
}

fn hash01(mut h: u64) -> f32 {
    h ^= h >> 33;
    h = h.wrapping_mul(0xFF51AFD7ED558CCD);
    h ^= h >> 33;
    ((h >> 32) as f32) / (u32::MAX as f32)
}

/// ライン: 動く斜線ストライプ
fn render_lines(t: f32) -> RgbaImage {
    RgbaImage::from_fn(GEN_W, GEN_H, |x, y| {
        let v = (((x as f32 + y as f32) * 0.35 - t * 120.0).sin() * 0.5 + 0.5).powf(2.0);
        let edge = (((x as f32 - y as f32 * 0.5) * 0.08 + t * 0.8).sin() * 0.5 + 0.5).powf(4.0);
        image::Rgba([
            (v * 255.0) as u8,
            (v * 140.0 + edge * 115.0) as u8,
            (edge * 255.0) as u8,
            255,
        ])
    })
}

/// カラーバー + 動く白枠 (出力チェック用)
fn render_bars(t: f32) -> RgbaImage {
    const COLORS: [[u8; 3]; 7] = [
        [192, 192, 192],
        [192, 192, 0],
        [0, 192, 192],
        [0, 192, 0],
        [192, 0, 192],
        [192, 0, 0],
        [0, 0, 192],
    ];
    RgbaImage::from_fn(GEN_W, GEN_H, |x, y| {
        let idx = (x as usize * 7 / GEN_W as usize).min(6);
        let mut c = COLORS[idx];
        // 下部に動く白い四角 (動き・遅延の目視確認用)
        let bx = (t * 160.0) as u32 % GEN_W;
        if x >= bx && x < bx + 40 && y > GEN_H - 60 && y < GEN_H - 20 {
            c = [255, 255, 255];
        }
        image::Rgba([c[0], c[1], c[2], 255])
    })
}

/// 簡易HSV→RGB (h,s,v: 0..1)
fn hsv(h: f32, s: f32, v: f32) -> image::Rgba<u8> {
    let i = (h * 6.0).floor() as i32;
    let f = h * 6.0 - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let tt = v * (1.0 - (1.0 - f) * s);
    let (r, g, b) = match i.rem_euclid(6) {
        0 => (v, tt, p),
        1 => (q, v, p),
        2 => (p, v, tt),
        3 => (p, q, v),
        4 => (tt, p, v),
        _ => (v, p, q),
    };
    image::Rgba([(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8, 255])
}
