//! プリセット保存/読込 (v0.1)。
//! `*.freemapper.json`: Project(形状・レイヤ) + UiState(素材・エフェクト・ファイルパス)。
//! schema不一致はエラー表示 (マイグレーションはv0.2以降)。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::effects::Effects;
use crate::generators::GeneratorKind;
use crate::slots::{SlotKind, SourceSlot};
use crate::surface::{RuntimeSurface, SurfaceMode};
use freemapper_core::Project;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveFile {
    pub schema: u32,
    pub project: Project,
    pub ui: UiState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurfaceData {
    pub id: String,
    pub name: String,
    pub visible: bool,
    pub mode: SurfaceMode,
    pub quad: [[f32; 2]; 4],
    pub mesh_cols: u32,
    pub mesh_rows: u32,
    pub mesh_points: Vec<[f32; 2]>,
    pub mask_polygon: Vec<[f32; 2]>,
    pub mask_invert: bool,
    /// v2.1追加 (欠番→1.0扱い)
    #[serde(default = "opacity_default")]
    pub opacity: f32,
    /// v2.2追加: 出力配置 (欠番→全面扱い)
    #[serde(default = "rect_default")]
    pub out_rect: [f32; 4],
    /// v3追加: 割当ソーススロットID (欠番→slot1扱い)
    #[serde(default = "slot1_default")]
    pub source_id: String,
}

fn opacity_default() -> f32 {
    1.0
}

fn rect_default() -> [f32; 4] {
    [0.0, 0.0, 1.0, 1.0]
}

fn slot1_default() -> String {
    "slot1".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotData {
    pub id: String,
    pub name: String,
    pub is_file: bool,
    /// gen時の種別 (file時は無視)
    pub generator: GeneratorKind,
    pub media_path: Option<String>,
    pub crop: [f32; 4],
    pub speed: f32,
    pub repeat: bool,
}

impl SlotData {
    pub fn from_slot(s: &SourceSlot) -> Self {
        let (is_file, generator) = match &s.kind {
            SlotKind::Generator(g) => (false, *g),
            SlotKind::File { .. } => (true, crate::generators::GeneratorKind::Waves),
        };
        Self {
            id: s.id.clone(),
            name: s.name.clone(),
            is_file,
            generator,
            media_path: match &s.kind {
                SlotKind::File { path, .. } => {
                    path.as_ref().map(|p| p.to_string_lossy().to_string())
                }
                _ => None,
            },
            crop: s.crop,
            speed: s.speed,
            repeat: s.repeat,
        }
    }

    /// 検証付き復元。ファイルが消えていたらジェネレーターに退避。
    pub fn into_slot(self, fallback_id: u32) -> SourceSlot {
        if self.is_file {
            if let Some(p) = self.media_path.as_ref() {
                let path = std::path::PathBuf::from(p);
                if let Ok(media) = crate::media::load_file(&path) {
                    let mut slot = SourceSlot::new_file(fallback_id, media, path);
                    slot.id = self.id;
                    slot.name = self.name;
                    slot.crop = self.crop;
                    slot.set_speed(if self.speed > 0.0 { self.speed } else { 1.0 });
                    slot.set_repeat(self.repeat);
                    if slot.is_video() && ((slot.speed - 1.0).abs() > 1e-3 || !slot.repeat) {
                        slot.respawn_video();
                    }
                    return slot;
                }
            }
        }
        let mut slot = SourceSlot::new_generator(fallback_id);
        slot.id = self.id;
        slot.name = self.name;
        if !self.is_file {
            if let SlotKind::Generator(g) = &mut slot.kind {
                *g = self.generator;
            }
        }
        slot.crop = self.crop;
        slot
    }
}

impl SurfaceData {
    pub fn from_runtime(s: &RuntimeSurface) -> Self {
        Self {
            id: s.id.clone(),
            name: s.name.clone(),
            visible: s.visible,
            mode: s.mode,
            quad: s.quad,
            mesh_cols: s.mesh.cols,
            mesh_rows: s.mesh.rows,
            mesh_points: s.mesh.points.clone(),
            mask_polygon: s.mask.polygon.clone(),
            mask_invert: s.mask.invert,
            opacity: s.opacity,
            out_rect: s.out_rect,
            source_id: s.source_id.clone(),
        }
    }

    /// 検証付きでRuntimeSurface化。不正値はデフォルトに倒す。
    pub fn into_runtime(self, fallback_id: u32) -> RuntimeSurface {
        let mut s = RuntimeSurface::new_quad(fallback_id, 0.0);
        s.id = self.id;
        s.name = self.name;
        s.visible = self.visible;
        s.mode = self.mode;
        s.quad = self.quad;
        let cols = self.mesh_cols.clamp(2, 16);
        let rows = self.mesh_rows.clamp(2, 16);
        s.mesh.ensure_size(cols, rows);
        if self.mesh_points.len() == s.mesh.points.len() {
            s.mesh.points = self.mesh_points;
        }
        if self.mask_polygon.len() >= 3 {
            s.mask.polygon = self.mask_polygon;
        }
        s.mask.invert = self.mask_invert;
        s.opacity = self.opacity.clamp(0.0, 1.0);
        s.out_rect = normalize_rect(self.out_rect);
        s.source_id = self.source_id;
        s
    }
}

/// 出力配置の正規化 (範囲外・潰れを補正)。
pub fn normalize_rect(r: [f32; 4]) -> [f32; 4] {
    let w = r[2].clamp(0.05, 1.0);
    let h = r[3].clamp(0.05, 1.0);
    [r[0].clamp(0.0, 1.0 - w), r[1].clamp(0.0, 1.0 - h), w, h]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiState {
    /// 3=ソーススロット制。旧ファイルはレガシーフィールドから移行。
    #[serde(default)]
    pub ui_version: u32,
    #[serde(default)]
    pub slots: Vec<SlotData>,
    #[serde(default)]
    pub next_slot_id: u32,
    #[serde(default)]
    pub surfaces: Vec<SurfaceData>,
    #[serde(default)]
    pub active: usize,
    #[serde(default)]
    pub next_id: u32,
    /// 出力キャンバス解像度 (0=旧ファイル→1920×1080扱い)
    #[serde(default)]
    pub out_w: u32,
    #[serde(default)]
    pub out_h: u32,
    /// 入力トリミング [x,y,w,h] (欠番→全面)
    #[serde(default)]
    pub crop: [f32; 4],
    /// 再生速度 (0=旧ファイル→1.0扱い)
    #[serde(default)]
    pub media_speed: f32,
    /// ループ再生 (欠番→true扱い)
    #[serde(default = "repeat_default")]
    pub loop_play: bool,
    // --- 以下レガシー(v1)単体フィールド: 移行用に保持、新規保存時も充填 ---
    pub surface_mode: SurfaceMode,
    pub quad: [[f32; 2]; 4],
    pub mesh_cols: u32,
    pub mesh_rows: u32,
    pub mesh_points: Vec<[f32; 2]>,
    pub mask_polygon: Vec<[f32; 2]>,
    pub mask_invert: bool,
    pub source_is_file: bool,
    pub generator: GeneratorKind,
    /// ファイルモード時のメディアパス (無ければジェネレーターにフォールバック)
    pub media_path: Option<String>,
    pub effects: Effects,
}

fn repeat_default() -> bool {
    true
}

pub fn save_dialog() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("FreeMapper preset", &["freemapper.json", "json"])
        .set_file_name("preset.freemapper.json")
        .save_file()
}

pub fn load_dialog() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("FreeMapper preset", &["freemapper.json", "json"])
        .pick_file()
}

pub fn save_to(path: &Path, data: &SaveFile) -> Result<(), String> {
    let json = serde_json::to_string_pretty(data).map_err(|e| format!("serialize: {e}"))?;
    // 原子書込: tmp→renameでクラッシュ時の破損防止 (SPEC §7)
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("write: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

pub fn load_from(path: &Path) -> Result<SaveFile, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    let data: SaveFile = serde_json::from_str(&text).map_err(|e| format!("parse: {e}"))?;
    if data.schema != freemapper_core::SCHEMA_VERSION {
        return Err(format!(
            "schema {} は非対応 (appは{}): v0.2のマイグレーション待ち",
            data.schema,
            freemapper_core::SCHEMA_VERSION
        ));
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_rect_clamps() {
        assert_eq!(normalize_rect([0.0, 0.0, 1.0, 1.0]), [0.0, 0.0, 1.0, 1.0]);
        // 潰れは最小化、はみ出しは引き戻し
        assert_eq!(normalize_rect([0.9, 0.9, 0.5, 0.5]), [0.5, 0.5, 0.5, 0.5]);
        assert_eq!(normalize_rect([0.0, 0.0, 0.0, 0.0]), [0.0, 0.0, 0.05, 0.05]);
    }

    #[test]
    fn slot_roundtrip_generator() {
        use crate::slots::SourceSlot;
        let mut s = SourceSlot::new_generator(3);
        if let crate::slots::SlotKind::Generator(g) = &mut s.kind {
            *g = crate::generators::GeneratorKind::Plasma;
        }
        s.crop = [0.1, 0.1, 0.8, 0.8];
        let data = SlotData::from_slot(&s);
        let json = serde_json::to_string(&data).unwrap();
        let back: SlotData = serde_json::from_str(&json).unwrap();
        let restored = back.into_slot(9);
        assert_eq!(restored.id, "slot3");
        assert_eq!(restored.crop, [0.1, 0.1, 0.8, 0.8]);
        assert!(matches!(
            restored.kind,
            crate::slots::SlotKind::Generator(crate::generators::GeneratorKind::Plasma)
        ));
    }
}
