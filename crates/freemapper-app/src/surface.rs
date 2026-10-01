//! サーフェス形状: Quad / Mesh / Mask (v0.1)。
//! - Mesh: グリッド制御点のドラッグ変形。Bezierハンドルはv0.2 (GPU化時)。
//! - Mask: ポリゴンでソースを切り抜き (CPU alpha適用、キャッシュ付き)。
//!   Feather(羽)はv0.2予定。

use image::RgbaImage;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SurfaceMode {
    #[default]
    Quad,
    Mesh,
    Mask,
}

// ---------- RuntimeSurface (複数サーフェス保持) ----------

/// エディタが保持する1サーフェス分。保存用 SurfaceData と相互変換する。
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeSurface {
    pub id: String,
    pub name: String,
    pub visible: bool,
    pub mode: SurfaceMode,
    pub quad: [[f32; 2]; 4],
    pub mesh: MeshState,
    pub mask: MaskState,
    /// 面不透明度 0..1 (頂点alphaでGPU合成・CPU負荷ゼロ)
    pub opacity: f32,
    /// ソフトエッジ [左,右,上,下] 0..0.5 (ブレンディング用フェザー幅・正規化)
    pub edge: [f32; 4],
    /// 出力段: キャンバス上の配置 [x,y,w,h] 正規化。入力段の形状とは独立。
    pub out_rect: [f32; 4],
    /// 入力段: 割当ソーススロットID
    pub source_id: String,
    /// File静止画モード用のマスク変更検出スナップショット
    mask_sig: (Vec<[f32; 2]>, bool),
}

impl RuntimeSurface {
    pub fn new_quad(id: u32, offset: f32) -> Self {
        let o = offset.clamp(0.0, 0.5);
        Self {
            id: format!("s{id}"),
            name: format!("S{id}"),
            visible: true,
            mode: SurfaceMode::Quad,
            quad: [
                [0.15 + o, 0.15 + o],
                [0.85 - o, 0.15 + o],
                [0.85 - o, 0.85 - o],
                [0.15 + o, 0.85 - o],
            ],
            mesh: MeshState::default(),
            mask: MaskState::default(),
            opacity: 1.0,
            edge: [0.0; 4],
            out_rect: [0.0, 0.0, 1.0, 1.0],
            source_id: "slot1".to_string(),
            mask_sig: (Vec::new(), false),
        }
    }

    pub fn reset_shape(&mut self) {
        self.quad = RuntimeSurface::new_quad(0, 0.0).quad;
        self.mesh = MeshState::new(self.mesh.cols, self.mesh.rows);
        self.mask = MaskState::default();
        self.mask_sig = (Vec::new(), false);
    }

    /// 重心を(0.5,0.5)へ移動 (形状・サイズ維持)。
    pub fn center_shape(&mut self) {
        let pts = self.control_points();
        if pts.is_empty() {
            return;
        }
        let n = pts.len() as f32;
        let cx = pts.iter().map(|p| p[0]).sum::<f32>() / n;
        let cy = pts.iter().map(|p| p[1]).sum::<f32>() / n;
        let moved: Vec<[f32; 2]> = pts
            .iter()
            .map(|p| {
                [
                    (p[0] + 0.5 - cx).clamp(0.0, 1.0),
                    (p[1] + 0.5 - cy).clamp(0.0, 1.0),
                ]
            })
            .collect();
        self.apply_control_points(&moved);
    }

    /// 全面フィット (Quad=全画面 / Mesh=全面グリッド / Mask=全面矩形)。
    pub fn fill_shape(&mut self) {
        match self.mode {
            SurfaceMode::Quad => {
                self.quad = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
            }
            SurfaceMode::Mesh => {
                let (c, r) = (self.mesh.cols, self.mesh.rows);
                self.mesh = MeshState::new(c, r);
                // reinitは0.1..0.9なので全面に伸ばす
                for j in 0..=r {
                    for i in 0..=c {
                        let k = self.mesh.idx(i, j);
                        self.mesh.points[k] = [i as f32 / c as f32, j as f32 / r as f32];
                    }
                }
            }
            SurfaceMode::Mask => {
                self.mask.polygon = vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
            }
        }
    }

    /// Mask編集中のポリゴン変化を検出 (変化時のみtrue + スナップショット更新)。
    pub fn mask_changed(&mut self) -> bool {
        if self.mode != SurfaceMode::Mask {
            return false;
        }
        let changed = self.mask_sig.0 != self.mask.polygon || self.mask_sig.1 != self.mask.invert;
        if changed {
            self.mask_sig = (self.mask.polygon.clone(), self.mask.invert);
        }
        changed
    }

    /// 制御点をVecで取得 (編集UI用)。
    pub fn control_points(&self) -> Vec<[f32; 2]> {
        match self.mode {
            SurfaceMode::Quad => self.quad.to_vec(),
            SurfaceMode::Mesh => self.mesh.points.clone(),
            SurfaceMode::Mask => self.mask.polygon.clone(),
        }
    }

    /// 編集UIからの書き戻し。
    pub fn apply_control_points(&mut self, pts: &[[f32; 2]]) {
        match self.mode {
            SurfaceMode::Quad => {
                if pts.len() == 4 {
                    self.quad = [pts[0], pts[1], pts[2], pts[3]];
                }
            }
            SurfaceMode::Mesh => {
                if pts.len() == self.mesh.points.len() {
                    self.mesh.points = pts.to_vec();
                }
            }
            SurfaceMode::Mask => {
                self.mask.polygon = pts.to_vec();
            }
        }
    }
}

// ---------- Mesh ----------

#[derive(Debug, Clone, PartialEq)]
pub struct MeshState {
    pub cols: u32,
    pub rows: u32,
    /// (cols+1)*(rows+1) points, row-major, normalized 0..1.
    pub points: Vec<[f32; 2]>,
}

impl Default for MeshState {
    fn default() -> Self {
        Self::new(8, 8)
    }
}

impl MeshState {
    pub fn new(cols: u32, rows: u32) -> Self {
        let mut s = Self {
            cols,
            rows,
            points: Vec::new(),
        };
        s.reinit();
        s
    }

    /// サイズ変更時はbilinear初期位置に作り直す (v0.1仕様)。
    pub fn ensure_size(&mut self, cols: u32, rows: u32) {
        let cols = cols.clamp(2, 16);
        let rows = rows.clamp(2, 16);
        if self.cols != cols || self.rows != rows {
            self.cols = cols;
            self.rows = rows;
            self.reinit();
        }
    }

    fn reinit(&mut self) {
        self.points.clear();
        for j in 0..=self.rows {
            for i in 0..=self.cols {
                self.points.push([
                    0.1 + 0.8 * (i as f32 / self.cols as f32),
                    0.1 + 0.8 * (j as f32 / self.rows as f32),
                ]);
            }
        }
    }

    pub fn idx(&self, i: u32, j: u32) -> usize {
        (j * (self.cols + 1) + i) as usize
    }
}

// ---------- Mask ----------

#[derive(Debug, Clone, PartialEq)]
pub struct MaskState {
    pub polygon: Vec<[f32; 2]>,
    pub invert: bool,
    // alpha cache: ポリゴン不変なら再計算しない (毎フレームの負荷対策)
    cache_key: Vec<[f32; 2]>,
    cache_invert: bool,
    cache_w: u32,
    cache_h: u32,
    cache_alpha: Vec<u8>,
}

impl Default for MaskState {
    fn default() -> Self {
        Self {
            polygon: vec![[0.2, 0.2], [0.8, 0.2], [0.8, 0.8], [0.2, 0.8]],
            invert: false,
            cache_key: Vec::new(),
            cache_invert: false,
            cache_w: 0,
            cache_h: 0,
            cache_alpha: Vec::new(),
        }
    }
}

impl MaskState {
    /// 最長辺の中点に頂点追加。
    pub fn add_point(&mut self) {
        if self.polygon.len() < 2 {
            return;
        }
        let mut best = 0;
        let mut best_d = 0.0f32;
        for (k, w) in self.polygon.windows(2).enumerate() {
            let d = dist(w[0], w[1]);
            if d > best_d {
                best_d = d;
                best = k;
            }
        }
        let a = self.polygon[best];
        let b = self.polygon[(best + 1) % self.polygon.len().max(1)];
        // 閉路の最終辺も候補にする
        let n = self.polygon.len();
        let (la, lb) = (self.polygon[n - 1], self.polygon[0]);
        if dist(la, lb) > best_d {
            let mid = [(la[0] + lb[0]) * 0.5, (la[1] + lb[1]) * 0.5];
            self.polygon.push(mid);
        } else {
            let mid = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
            self.polygon.insert(best + 1, mid);
        }
    }

    pub fn remove_point(&mut self) {
        if self.polygon.len() > 3 {
            self.polygon.pop();
        }
    }

    /// 画像のalphaにマスク適用 (インプレース)。範囲外→透明。
    pub fn apply(&mut self, img: &mut RgbaImage) {
        let (w, h) = (img.width(), img.height());
        let dirty = self.cache_w != w
            || self.cache_h != h
            || self.cache_invert != self.invert
            || self.cache_key != self.polygon;
        if dirty {
            self.rebuild_cache(w, h);
        }
        for (px, a) in img.pixels_mut().zip(self.cache_alpha.iter()) {
            px.0[3] = ((px.0[3] as u16 * *a as u16) / 255) as u8;
        }
    }

    fn rebuild_cache(&mut self, w: u32, h: u32) {
        self.cache_key = self.polygon.clone();
        self.cache_invert = self.invert;
        self.cache_w = w;
        self.cache_h = h;
        self.cache_alpha = vec![0u8; (w * h) as usize];
        if self.polygon.len() < 3 {
            return;
        }
        for y in 0..h {
            for x in 0..w {
                let p = [(x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32];
                let inside = point_in_polygon(p, &self.polygon);
                let keep = if self.invert { !inside } else { inside };
                self.cache_alpha[(y * w + x) as usize] = if keep { 255 } else { 0 };
            }
        }
    }
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// ソフトエッジ適用: edge=[左,右,上,下]幅でalphaを滑らかに落とす (インプレース)。
/// マルチプロジェクタのブレンディング用。幅0なら何もしない。
pub fn apply_edge_feather(img: &mut RgbaImage, edge: [f32; 4]) {
    if edge.iter().all(|e| *e <= 0.0) {
        return;
    }
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return;
    }
    for (x, y, px) in img.enumerate_pixels_mut() {
        let fx = (x as f32 + 0.5) / w as f32;
        let fy = (y as f32 + 0.5) / h as f32;
        let mut k = 1.0f32;
        // 各辺: 端からの距離/幅 をsmoothstepで0..1化
        if edge[0] > 0.0 {
            k *= smoothstep((fx / edge[0]).clamp(0.0, 1.0));
        }
        if edge[1] > 0.0 {
            k *= smoothstep(((1.0 - fx) / edge[1]).clamp(0.0, 1.0));
        }
        if edge[2] > 0.0 {
            k *= smoothstep((fy / edge[2]).clamp(0.0, 1.0));
        }
        if edge[3] > 0.0 {
            k *= smoothstep(((1.0 - fy) / edge[3]).clamp(0.0, 1.0));
        }
        if k < 1.0 {
            px.0[3] = (px.0[3] as f32 * k) as u8;
        }
    }
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Quad双線形補間: quad=[TL,TR,BR,BL], u,v in 0..1。
/// 2三角形affine分割と違い、対角線の継ぎ目が出ない。
/// 描画側はこの補間で細分グリッド化する (真の射影変換はGPUシェーダ化時の課題)。
pub fn bilinear_quad(quad: [[f32; 2]; 4], u: f32, v: f32) -> [f32; 2] {
    let (a, b, c, d) = (quad[0], quad[1], quad[2], quad[3]);
    let (iu, iv) = (1.0 - u, 1.0 - v);
    [
        a[0] * iu * iv + b[0] * u * iv + c[0] * u * v + d[0] * iu * v,
        a[1] * iu * iv + b[1] * u * iv + c[1] * u * v + d[1] * iu * v,
    ]
}

/// Ray casting法。
fn point_in_polygon(p: [f32; 2], poly: &[[f32; 2]]) -> bool {
    let mut inside = false;
    let n = poly.len();
    let mut j = n - 1;
    for i in 0..n {
        let xi = poly[i][0];
        let yi = poly[i][1];
        let xj = poly[j][0];
        let yj = poly[j][1];
        if ((yi > p[1]) != (yj > p[1])) && (p[0] < (xj - xi) * (p[1] - yi) / (yj - yi) + xi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_contains_center() {
        let sq = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        assert!(point_in_polygon([0.5, 0.5], &sq));
        assert!(!point_in_polygon([1.5, 0.5], &sq));
    }

    #[test]
    fn mesh_reinit_count() {
        let m = MeshState::new(4, 3);
        assert_eq!(m.points.len(), 5 * 4);
    }

    #[test]
    fn feather_fades_edges() {
        let mut img = RgbaImage::from_pixel(100, 100, image::Rgba([255, 255, 255, 255]));
        apply_edge_feather(&mut img, [0.2, 0.0, 0.0, 0.0]);
        // 左端は透明寄り、中央は不透明維持
        assert!(img.get_pixel(0, 50).0[3] < 128);
        assert_eq!(img.get_pixel(50, 50).0[3], 255);
    }

    #[test]
    fn bilinear_corners_and_center() {
        let q = [[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]];
        assert_eq!(bilinear_quad(q, 0.0, 0.0), [0.0, 0.0]);
        assert_eq!(bilinear_quad(q, 1.0, 0.0), [2.0, 0.0]);
        assert_eq!(bilinear_quad(q, 1.0, 1.0), [2.0, 2.0]);
        assert_eq!(bilinear_quad(q, 0.0, 1.0), [0.0, 2.0]);
        assert_eq!(bilinear_quad(q, 0.5, 0.5), [1.0, 1.0]);
        // 台形でも辺中点は両端の中点に一致 (継ぎ目なしの条件)
        let trap = [[0.0, 0.0], [1.0, 0.0], [0.8, 1.0], [0.2, 1.0]];
        let mid = bilinear_quad(trap, 0.5, 1.0);
        assert!((mid[0] - 0.5).abs() < 1e-6 && (mid[1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn center_and_fill() {
        let mut s = RuntimeSurface::new_quad(1, 0.0);
        s.quad = [[0.0, 0.0], [0.2, 0.0], [0.2, 0.2], [0.0, 0.2]];
        s.center_shape();
        let cx = s.quad.iter().map(|p| p[0]).sum::<f32>() / 4.0;
        assert!((cx - 0.5).abs() < 1e-6);
        s.fill_shape();
        assert_eq!(s.quad[2], [1.0, 1.0]);
    }
}
