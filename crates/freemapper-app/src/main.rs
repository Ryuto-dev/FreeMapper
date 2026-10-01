//! FreeMapper v0.1 spike binary.
//! eframe (glow backend) + egui + ja/en i18n + Quad warp + media + effects + output.
//!
//! Run: `cargo run -p freemapper-app`

mod effects;
mod generators;
mod media;
mod osc;
mod persist;
mod slots;
mod surface;
mod syphon_out;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui;
use freemapper_core::{Project, SourceScheme};

use effects::{to_color_image, Effects};
use generators::GeneratorKind;
use media::Media;
use osc::{OscCmd, OscServer, DEFAULT_OSC_PORT};
use persist::{SaveFile, UiState};
use slots::{SlotKind, SourceSlot};
use surface::{bilinear_quad, MeshState, RuntimeSurface, SurfaceMode};
use syphon_out::{SyphonOut, SyphonStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Ja,
    En,
}

struct I18n {
    lang: Lang,
    ja: HashMap<String, String>,
    en: HashMap<String, String>,
}

impl I18n {
    fn load() -> Self {
        let ja: HashMap<String, String> =
            serde_json::from_str(include_str!("../../../locales/ja.json")).unwrap();
        let en: HashMap<String, String> =
            serde_json::from_str(include_str!("../../../locales/en.json")).unwrap();
        Self {
            lang: Lang::Ja,
            ja,
            en,
        }
    }

    fn t<'a>(&'a self, key: &'a str) -> &'a str {
        let map = match self.lang {
            Lang::Ja => &self.ja,
            Lang::En => &self.en,
        };
        map.get(key).map(|s| s.as_str()).unwrap_or(key)
    }
}

/// 出力viewportと共有する状態 ('static制約のためArc経由)。
#[derive(Clone, Default)]
struct SharedItem {
    mode: SurfaceMode,
    quad: [[f32; 2]; 4],
    mesh: MeshState,
    texture: Option<egui::TextureHandle>,
    opacity: f32,
    out_rect: [f32; 4],
}

#[derive(Clone, Default)]
struct SharedOutput {
    items: Arc<Mutex<Vec<SharedItem>>>,
}

/// Undo/Redo用スナップショット (サーフェス形状系のみ)。
#[derive(Clone, PartialEq)]
struct HistorySnap {
    surfaces: Vec<RuntimeSurface>,
    active: usize,
    next_id: u32,
}

struct MapperApp {
    i18n: I18n,
    dragging: Option<usize>,
    project: Project,
    surfaces: Vec<RuntimeSurface>,
    active: usize,
    next_id: u32,
    /// surfacesと並行: 各サーフェス用テクスチャ (Maskは個別、他は同内容)
    textures: Vec<Option<egui::TextureHandle>>,
    /// 入力段: ソーススロット (面はsource_idで参照)
    slots: Vec<SourceSlot>,
    next_slot_id: u32,
    media_error: Option<String>,
    effects: Effects,
    fx_dirty: bool,
    fps_ema: f32,
    started: Instant,
    show_output: Arc<AtomicBool>,
    output_fullscreen: bool,
    /// 出力キャンバス解像度 (右ペインで管理・出力段の正)
    out_w: u32,
    out_h: u32,
    shared: SharedOutput,
    save_message: Option<String>,
    save_path: Option<PathBuf>,
    syphon: SyphonOut,
    osc: OscServer,
    osc_enabled: bool,
    osc_port: u16,
    // --- 編集系 (MadMapper基礎) ---
    history: Vec<HistorySnap>,
    hist_idx: usize,
    hist_pending: bool,
    selected: Option<usize>,
    drag_moved: bool,
    press_candidate: Option<usize>,
    snap_enabled: bool,
    show_grid: bool,
    // --- 出力系 ---
    blackout: bool,
    freeze: bool,
    // --- 入力系 ---
    speed_pending: bool,
    last_autosave: Instant,
    autosave_count: u32,
    autosave_label: String,
}

impl Default for MapperApp {
    fn default() -> Self {
        Self {
            i18n: I18n::load(),
            dragging: None,
            project: Project::default(),
            surfaces: vec![RuntimeSurface::new_quad(1, 0.0)],
            active: 0,
            next_id: 2,
            textures: vec![None],
            slots: vec![SourceSlot::new_generator(1)],
            next_slot_id: 2,
            media_error: None,
            effects: Effects::default(),
            fx_dirty: true,
            fps_ema: 60.0,
            started: Instant::now(),
            show_output: Arc::new(AtomicBool::new(false)),
            output_fullscreen: false,
            out_w: 1920,
            out_h: 1080,
            shared: SharedOutput::default(),
            save_message: None,
            save_path: None,
            syphon: SyphonOut::default(),
            osc: OscServer::default(),
            osc_enabled: false,
            osc_port: DEFAULT_OSC_PORT,
            history: Vec::new(),
            hist_idx: 0,
            hist_pending: false,
            selected: None,
            drag_moved: false,
            press_candidate: None,
            snap_enabled: true,
            show_grid: true,
            blackout: false,
            freeze: false,
            speed_pending: false,
            last_autosave: Instant::now(),
            autosave_count: 0,
            autosave_label: String::new(),
        }
    }
}

impl MapperApp {
    fn active_surface(&self) -> &RuntimeSurface {
        &self.surfaces[self.active.min(self.surfaces.len().saturating_sub(1))]
    }

    fn active_surface_mut(&mut self) -> &mut RuntimeSurface {
        let i = self.active.min(self.surfaces.len().saturating_sub(1));
        &mut self.surfaces[i]
    }

    /// アクティブ面の割当スロット (無ければ先頭)。
    fn active_slot(&self) -> &SourceSlot {
        let id = self.active_surface().source_id.clone();
        self.slots
            .iter()
            .find(|s| s.id == id)
            .unwrap_or(&self.slots[0])
    }

    fn active_slot_mut(&mut self) -> &mut SourceSlot {
        let id = self.active_surface().source_id.clone();
        let idx = self.slots.iter().position(|s| s.id == id).unwrap_or(0);
        &mut self.slots[idx]
    }

    /// ジェネレータースロットを確保 (無ければ作成) してIDを返す。
    fn ensure_generator_slot(&mut self) -> String {
        if let Some(s) = self.slots.iter().find(|s| !s.is_file()) {
            return s.id.clone();
        }
        let id = self.next_slot_id;
        self.next_slot_id += 1;
        let slot = SourceSlot::new_generator(id);
        let sid = slot.id.clone();
        self.slots.push(slot);
        sid
    }

    /// アクティブスロットのジェネレーター種別変更。
    fn set_active_gen(&mut self, g: GeneratorKind) {
        if let SlotKind::Generator(gg) = &mut self.active_slot_mut().kind {
            *gg = g;
        }
        self.fx_dirty = true;
    }

    fn set_active_paused(&mut self, paused: bool) {
        if let SlotKind::File {
            media: Media::Video { stream, .. },
            ..
        } = &mut self.active_slot_mut().kind
        {
            stream.paused = paused;
        }
    }

    fn set_active_speed(&mut self, sp: f32) {
        let is_video = self.active_slot().is_video();
        self.active_slot_mut().set_speed(sp);
        if is_video {
            // 確定時(ポインタ release)にspawnし直し
            self.speed_pending = true;
        }
    }

    fn set_active_repeat(&mut self, repeat: bool) {
        let is_video = self.active_slot().is_video();
        self.active_slot_mut().set_repeat(repeat);
        if is_video {
            self.respawn_video();
        }
    }

    fn reset_quad(&mut self) {
        self.active_surface_mut().reset_shape();
    }

    fn add_surface(&mut self) {
        let id = self.next_id;
        self.next_id += 1;
        let offset = 0.05 * ((id % 5) as f32);
        self.surfaces.push(RuntimeSurface::new_quad(id, offset));
        self.textures.push(None);
        self.active = self.surfaces.len() - 1;
        self.fx_dirty = true;
        self.sync_to_project();
        self.push_history();
    }

    fn remove_surface(&mut self) {
        if self.surfaces.len() <= 1 {
            return;
        }
        self.surfaces
            .remove(self.active.min(self.surfaces.len() - 1));
        self.textures
            .remove(self.active.min(self.textures.len() - 1));
        self.active = self.active.min(self.surfaces.len() - 1);
        self.sync_to_project();
        self.push_history();
    }

    /// アクティブ面を前面へ (描画順=重ね順)。
    fn surface_forward(&mut self) {
        let n = self.surfaces.len();
        if self.active + 1 < n {
            self.surfaces.swap(self.active, self.active + 1);
            self.textures.swap(self.active, self.active + 1);
            self.active += 1;
            self.sync_to_project();
            self.push_history();
        }
    }

    /// アクティブ面を背面へ。
    fn surface_backward(&mut self) {
        if self.active > 0 {
            self.surfaces.swap(self.active, self.active - 1);
            self.textures.swap(self.active, self.active - 1);
            self.active -= 1;
            self.sync_to_project();
            self.push_history();
        }
    }

    /// アクティブ面を複製 (少しずらして前面に)。
    fn duplicate_surface(&mut self) {
        let mut s = self.active_surface().clone();
        let id = self.next_id;
        self.next_id += 1;
        s.id = format!("s{id}");
        s.name = format!("S{id}");
        // 少しずらして区別
        let mut pts = s.control_points();
        for p in &mut pts {
            p[0] = (p[0] + 0.03).clamp(0.0, 1.0);
            p[1] = (p[1] + 0.03).clamp(0.0, 1.0);
        }
        s.apply_control_points(&pts);
        self.surfaces.push(s);
        self.textures.push(None);
        self.active = self.surfaces.len() - 1;
        self.fx_dirty = true;
        self.sync_to_project();
        self.push_history();
    }

    // ---------- Undo/Redo ----------

    fn snapshot(&self) -> HistorySnap {
        HistorySnap {
            surfaces: self.surfaces.clone(),
            active: self.active,
            next_id: self.next_id,
        }
    }

    fn ensure_history(&mut self) {
        if self.history.is_empty() {
            let snap = self.snapshot();
            self.history.push(snap);
            self.hist_idx = 0;
        }
    }

    /// 同一内容の連続pushを避ける。
    fn push_history(&mut self) {
        self.ensure_history();
        let snap = self.snapshot();
        if self.history.get(self.hist_idx) == Some(&snap) {
            return;
        }
        self.history.truncate(self.hist_idx + 1);
        self.history.push(snap);
        if self.history.len() > 100 {
            self.history.remove(0);
        }
        self.hist_idx = self.history.len() - 1;
    }

    fn apply_snap(&mut self, snap: HistorySnap) {
        self.surfaces = snap.surfaces;
        self.active = snap.active.min(self.surfaces.len().saturating_sub(1));
        self.next_id = snap.next_id;
        self.dragging = None;
        self.selected = None;
        self.textures = vec![None; self.surfaces.len()];
        self.fx_dirty = true;
        self.sync_to_project();
    }

    fn undo(&mut self) {
        self.ensure_history();
        if self.hist_idx > 0 {
            self.hist_idx -= 1;
            let snap = self.history[self.hist_idx].clone();
            self.apply_snap(snap);
        }
    }

    fn redo(&mut self) {
        self.ensure_history();
        if self.hist_idx + 1 < self.history.len() {
            self.hist_idx += 1;
            let snap = self.history[self.hist_idx].clone();
            self.apply_snap(snap);
        }
    }

    /// 動画の速度・ループ変更時にspawnし直す (アクティブスロット)。
    fn respawn_video(&mut self) {
        if self.active_slot_mut().respawn_video() {
            self.fx_dirty = true;
        } else {
            self.media_error = Some(self.i18n.t("preset.error").to_string());
        }
    }

    fn sync_to_project(&mut self) {
        // 全サーフェスをprojectに反映
        self.project.surfaces = self
            .surfaces
            .iter()
            .map(|s| freemapper_core::Surface {
                id: s.id.clone(),
                kind: match s.mode {
                    SurfaceMode::Quad => {
                        freemapper_core::SurfaceKind::Quad(freemapper_core::Quad { points: s.quad })
                    }
                    SurfaceMode::Mesh => {
                        freemapper_core::SurfaceKind::Mesh(freemapper_core::Mesh {
                            cols: s.mesh.cols,
                            rows: s.mesh.rows,
                            points: s.mesh.points.clone(),
                        })
                    }
                    SurfaceMode::Mask => {
                        freemapper_core::SurfaceKind::Mask(freemapper_core::Mask {
                            polygon: s.mask.polygon.clone(),
                            feather: 0.0,
                            invert: s.mask.invert,
                        })
                    }
                },
            })
            .collect();
        // レイヤーは面と1:1で再構築 (面の割当ソースを記録)
        self.project.layers = self
            .surfaces
            .iter()
            .map(|s| {
                let uri = self
                    .slots
                    .iter()
                    .find(|sl| sl.id == s.source_id)
                    .map(|sl| match &sl.kind {
                        SlotKind::Generator(g) => g.uri().to_string(),
                        SlotKind::File { media, .. } => format!("file://{}", media.name()),
                    })
                    .unwrap_or_else(|| "slot://missing".to_string());
                freemapper_core::Layer {
                    id: format!("l_{}", s.id),
                    source: freemapper_core::SourceUri(uri),
                    surface: s.id.clone(),
                    opacity: s.opacity,
                }
            })
            .collect();
    }

    /// ファイルを開いて新規スロット化＋アクティブ面に割当。
    fn open_file_dialog(&mut self) {
        let Some(path) = media::load_dialog() else {
            return;
        };
        match media::load_file(&path) {
            Ok(m) => {
                let id = self.next_slot_id;
                self.next_slot_id += 1;
                let short = m.name().split(" (").next().unwrap_or("file").to_string();
                let mut slot = SourceSlot::new_file(id, m, path);
                slot.name = short;
                let sid = slot.id.clone();
                self.slots.push(slot);
                self.active_surface_mut().source_id = sid;
                self.media_error = None;
                self.fx_dirty = true;
                self.sync_to_project();
                self.push_history();
            }
            Err(e) => {
                self.media_error = Some(e);
            }
        }
    }

    /// ジェネレータースロットを追加＋アクティブ面に割当。
    fn add_generator_slot(&mut self) {
        let id = self.next_slot_id;
        self.next_slot_id += 1;
        let slot = SourceSlot::new_generator(id);
        let sid = slot.id.clone();
        self.slots.push(slot);
        self.active_surface_mut().source_id = sid;
        self.fx_dirty = true;
        self.sync_to_project();
        self.push_history();
    }

    /// スロット削除 (使用中の面は先頭スロットへ退避)。
    fn remove_slot(&mut self, sid: &str) {
        if self.slots.len() <= 1 {
            return;
        }
        if let Some(pos) = self.slots.iter().position(|s| s.id == sid) {
            self.slots.remove(pos);
            let fallback = self.slots[0].id.clone();
            for s in &mut self.surfaces {
                if s.source_id == sid {
                    s.source_id = fallback.clone();
                }
            }
            self.fx_dirty = true;
            self.sync_to_project();
            self.push_history();
        }
    }

    fn collect_save(&mut self) -> SaveFile {
        self.sync_to_project();
        let active = self.active_surface().clone();
        // レガシー充填用: 先頭スロットの情報
        let first = self.slots.first();
        let (legacy_is_file, legacy_gen, legacy_path, legacy_crop, legacy_speed, legacy_repeat) =
            match first {
                Some(s) => match &s.kind {
                    SlotKind::Generator(g) => (false, *g, None, s.crop, s.speed, s.repeat),
                    SlotKind::File { path, .. } => (
                        true,
                        GeneratorKind::Waves,
                        path.as_ref().map(|p| p.to_string_lossy().to_string()),
                        s.crop,
                        s.speed,
                        s.repeat,
                    ),
                },
                None => (
                    false,
                    GeneratorKind::Waves,
                    None,
                    [0.0, 0.0, 1.0, 1.0],
                    1.0,
                    true,
                ),
            };
        SaveFile {
            schema: freemapper_core::SCHEMA_VERSION,
            project: self.project.clone(),
            ui: UiState {
                ui_version: 3,
                slots: self
                    .slots
                    .iter()
                    .map(persist::SlotData::from_slot)
                    .collect(),
                next_slot_id: self.next_slot_id,
                surfaces: self
                    .surfaces
                    .iter()
                    .map(persist::SurfaceData::from_runtime)
                    .collect(),
                active: self.active,
                next_id: self.next_id,
                out_w: self.out_w,
                out_h: self.out_h,
                crop: legacy_crop,
                media_speed: legacy_speed,
                loop_play: legacy_repeat,
                // レガシー充填 (v1/v2ファイル互換・移行用)
                surface_mode: active.mode,
                quad: active.quad,
                mesh_cols: active.mesh.cols,
                mesh_rows: active.mesh.rows,
                mesh_points: active.mesh.points.clone(),
                mask_polygon: active.mask.polygon.clone(),
                mask_invert: active.mask.invert,
                source_is_file: legacy_is_file,
                generator: legacy_gen,
                media_path: legacy_path,
                effects: self.effects.clone(),
            },
        }
    }

    fn do_save(&mut self) {
        let Some(path) = persist::save_dialog() else {
            return;
        };
        let data = self.collect_save();
        match persist::save_to(&path, &data) {
            Ok(()) => {
                self.save_path = Some(path.clone());
                self.save_message = Some(format!(
                    "{}: {}",
                    self.i18n.t("preset.saved"),
                    path.to_string_lossy()
                ));
            }
            Err(e) => {
                self.save_message = Some(format!("{}: {e}", self.i18n.t("preset.error")));
            }
        }
    }

    fn do_load(&mut self) {
        let Some(path) = persist::load_dialog() else {
            return;
        };
        match persist::load_from(&path) {
            Ok(data) => {
                self.restore(data);
                self.save_path = Some(path.clone());
                self.save_message = Some(format!(
                    "{}: {}",
                    self.i18n.t("preset.loaded"),
                    path.to_string_lossy()
                ));
            }
            Err(e) => {
                self.save_message = Some(format!("{}: {e}", self.i18n.t("preset.error")));
            }
        }
    }

    fn restore(&mut self, data: SaveFile) {
        self.dragging = None;
        self.selected = None;
        // v3: スロット + 複数サーフェス / 旧: レガシー単体から移行
        if data.ui.ui_version >= 3 && !data.ui.slots.is_empty() {
            let n = data.ui.slots.len();
            self.slots = data
                .ui
                .slots
                .into_iter()
                .enumerate()
                .map(|(i, s)| s.into_slot(i as u32 + 1))
                .collect();
            self.next_slot_id = data.ui.next_slot_id.max(n as u32 + 1).max(2);
        } else {
            // レガシー移行: 単一スロットを構築
            let mut slot = SourceSlot::new_generator(1);
            if data.ui.source_is_file {
                let ok = data
                    .ui
                    .media_path
                    .as_ref()
                    .map(|p| {
                        let path = PathBuf::from(p);
                        match media::load_file(&path) {
                            Ok(m) => {
                                slot = SourceSlot::new_file(1, m, path);
                                true
                            }
                            Err(_) => false,
                        }
                    })
                    .unwrap_or(false);
                if !ok {
                    self.media_error = Some(self.i18n.t("preset.media_missing").to_string());
                }
            }
            if !slot.is_file() {
                if let SlotKind::Generator(g) = &mut slot.kind {
                    *g = data.ui.generator;
                }
            }
            slot.crop = data.ui.crop;
            slot.speed = if data.ui.media_speed > 0.0 {
                data.ui.media_speed.clamp(0.25, 2.0)
            } else {
                1.0
            };
            slot.repeat = data.ui.loop_play;
            if slot.is_video() && ((slot.speed - 1.0).abs() > 1e-3 || !slot.repeat) {
                slot.respawn_video();
            }
            // Animated速度も反映
            if let SlotKind::File {
                media: Media::Animated { speed, repeat, .. },
                ..
            } = &mut slot.kind
            {
                *speed = slot.speed;
                *repeat = slot.repeat;
            }
            self.slots = vec![slot];
            self.next_slot_id = 2;
            self.media_error = None;
        }
        // 面の復元 (v3はsource_id付き、旧はslot1へ)
        if data.ui.ui_version >= 3 && !data.ui.surfaces.is_empty() {
            let n = data.ui.surfaces.len();
            self.surfaces = data
                .ui
                .surfaces
                .into_iter()
                .enumerate()
                .map(|(i, s)| s.into_runtime(i as u32 + 1))
                .collect();
            self.active = data.ui.active.min(n - 1);
            self.next_id = data.ui.next_id.max(n as u32 + 1).max(2);
        } else {
            let mut s = RuntimeSurface::new_quad(1, 0.0);
            s.mode = data.ui.surface_mode;
            s.quad = data.ui.quad;
            let cols = data.ui.mesh_cols.clamp(2, 16);
            let rows = data.ui.mesh_rows.clamp(2, 16);
            s.mesh.ensure_size(cols, rows);
            if data.ui.mesh_points.len() == s.mesh.points.len() {
                s.mesh.points = data.ui.mesh_points;
            }
            if data.ui.mask_polygon.len() >= 3 {
                s.mask.polygon = data.ui.mask_polygon;
            }
            s.mask.invert = data.ui.mask_invert;
            self.surfaces = vec![s];
            self.active = 0;
            self.next_id = 2;
        }
        // 参照切れsource_idの修復
        {
            let fallback = self.slots[0].id.clone();
            for s in &mut self.surfaces {
                if !self.slots.iter().any(|sl| sl.id == s.source_id) {
                    s.source_id = fallback.clone();
                }
            }
        }
        self.textures = vec![None; self.surfaces.len()];
        if data.ui.out_w > 0 && data.ui.out_h > 0 {
            self.out_w = data.ui.out_w;
            self.out_h = data.ui.out_h;
        }
        self.effects = data.ui.effects;
        self.project = data.project;
        self.fx_dirty = true;
        self.sync_to_project();
    }

    /// 現在のbase画像を決め、必要ならエフェクト適用→各サーフェスtexture更新。
    /// 流れ (スロット毎): poll → base → crop → effects → 面毎にmask/feather → upload。
    fn update_texture(&mut self, ctx: &egui::Context) {
        let t = self.started.elapsed().as_secs_f32();
        // 各スロットのpoll (Video pipe drain)
        for slot in &mut self.slots {
            slot.poll();
        }
        // スロット毎の処理要否
        let mut slot_need: Vec<bool> = Vec::with_capacity(self.slots.len());
        for slot in &mut self.slots {
            slot_need.push(slot.changed() || self.fx_dirty);
        }
        if self.textures.iter().any(|t| t.is_none()) {
            for n in slot_need.iter_mut() {
                *n = true;
            }
        }
        // Mask編集中のポリゴン変化でも再処理 (サーフェス毎の署名比較・安価)
        for s in &mut self.surfaces {
            if s.mask_changed() {
                let sid = s.source_id.clone();
                for (j, slot) in self.slots.iter().enumerate() {
                    if slot.id == sid {
                        slot_need[j] = true;
                    }
                }
            }
        }
        // スロット毎の共有画像を構築 (必要な分だけ)
        let mut slot_imgs: Vec<Option<image::RgbaImage>> = vec![None; self.slots.len()];
        for (j, slot) in self.slots.iter().enumerate() {
            if !slot_need[j] {
                continue;
            }
            if let Some(base) = slot.base(t) {
                let cropped = apply_crop(&base, slot.crop);
                slot_imgs[j] = Some(self.effects.apply(&cropped));
            }
        }
        if slot_imgs.iter().all(|o| o.is_none()) && self.textures.iter().all(|t| t.is_some()) {
            return;
        }
        self.fx_dirty = false;
        // Syphon配信 (アクティブ面のスロット画像・マスク前を送出)
        if self.syphon.enabled {
            let sid = self.active_surface().source_id.clone();
            if let Some(pos) = self.slots.iter().position(|s| s.id == sid) {
                if let Some(img) = &slot_imgs[pos] {
                    let raw = img.clone().into_raw();
                    let (w, h) = (img.width(), img.height());
                    self.syphon.push(&raw, w, h);
                }
            }
        }
        for (i, s) in self.surfaces.iter_mut().enumerate() {
            if !s.visible {
                continue;
            }
            let pos = self
                .slots
                .iter()
                .position(|sl| sl.id == s.source_id)
                .unwrap_or(0);
            let base = match &slot_imgs[pos] {
                Some(img) => img.clone(),
                None => continue, // 今回更新なし (既存texture維持)
            };
            let mut img = base;
            if s.mode == SurfaceMode::Mask {
                s.mask.apply(&mut img);
            }
            // ソフトエッジ (Mask有無に関わらず面別に適用)
            if s.edge.iter().any(|e| *e > 0.0) {
                surface::apply_edge_feather(&mut img, s.edge);
            }
            let color = to_color_image(&img);
            let name = format!("freemapper-s{i}");
            match &mut self.textures[i] {
                Some(h) => h.set(color, egui::TextureOptions::LINEAR),
                None => {
                    self.textures[i] =
                        Some(ctx.load_texture(name, color, egui::TextureOptions::LINEAR));
                }
            }
        }
    }
}

/// ワープ済みQuadを描画。texture無し(=未読込)はグラデーション代替。
/// opacityは頂点alphaでGPU合成 (CPU負荷ゼロ)。
/// 2三角形affine分割は対角線に継ぎ目が出るため、24分割グリッド＋双線形補間で一体変形する。
fn draw_quad(
    painter: &egui::Painter,
    rect: egui::Rect,
    quad: [[f32; 2]; 4],
    tex: Option<&egui::TextureHandle>,
    opacity: f32,
) {
    const N: usize = 24;
    let to_screen = |p: [f32; 2]| {
        egui::Pos2::new(
            rect.min.x + p[0] * rect.width(),
            rect.min.y + p[1] * rect.height(),
        )
    };
    // 外周 (ハンドル用・アウトライン用)
    let corners = [
        to_screen(quad[0]),
        to_screen(quad[1]),
        to_screen(quad[2]),
        to_screen(quad[3]),
    ];
    let grid_pos = |i: usize, j: usize| {
        let u = i as f32 / N as f32;
        let v = j as f32 / N as f32;
        to_screen(bilinear_quad(quad, u, v))
    };
    let at = |i: usize, j: usize| (j * (N + 1) + i) as u32;
    match tex {
        Some(h) => {
            let mut m = egui::Mesh::with_texture(h.id());
            let tint = egui::Color32::from_rgba_unmultiplied(
                255,
                255,
                255,
                (opacity.clamp(0.0, 1.0) * 255.0) as u8,
            );
            for j in 0..=N {
                for i in 0..=N {
                    m.vertices.push(egui::epaint::Vertex {
                        pos: grid_pos(i, j),
                        uv: egui::Pos2::new(i as f32 / N as f32, j as f32 / N as f32),
                        color: tint,
                    });
                }
            }
            for j in 0..N {
                for i in 0..N {
                    m.indices
                        .extend_from_slice(&[at(i, j), at(i + 1, j), at(i + 1, j + 1)]);
                    m.indices
                        .extend_from_slice(&[at(i, j), at(i + 1, j + 1), at(i, j + 1)]);
                }
            }
            painter.add(egui::Shape::mesh(m));
        }
        None => {
            // 未読込時は角色の双線形グラデーション (継ぎ目なし)
            let k = opacity.clamp(0.0, 1.0);
            let corner_cols = [
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
                [1.0, 1.0, 0.0],
            ];
            let mut mesh = egui::Mesh::default();
            for j in 0..=N {
                for i in 0..=N {
                    let u = i as f32 / N as f32;
                    let v = j as f32 / N as f32;
                    let c = bilinear_rgb(corner_cols, u, v);
                    mesh.colored_vertex(
                        grid_pos(i, j),
                        egui::Color32::from_rgb(
                            (c[0] * 255.0 * k) as u8,
                            (c[1] * 255.0 * k) as u8,
                            (c[2] * 255.0 * k) as u8,
                        ),
                    );
                }
            }
            for j in 0..N {
                for i in 0..N {
                    mesh.add_triangle(at(i, j), at(i + 1, j), at(i + 1, j + 1));
                    mesh.add_triangle(at(i, j), at(i + 1, j + 1), at(i, j + 1));
                }
            }
            painter.add(egui::Shape::mesh(mesh));
        }
    }
    painter.add(egui::Shape::closed_line(
        corners.to_vec(),
        egui::Stroke::new(1.5_f32, egui::Color32::WHITE),
    ));
}

/// RGB3ch版の双線形補間 (グラデーション代替用)。
fn bilinear_rgb(c: [[f32; 3]; 4], u: f32, v: f32) -> [f32; 3] {
    let (iu, iv) = (1.0 - u, 1.0 - v);
    [
        c[0][0] * iu * iv + c[1][0] * u * iv + c[2][0] * u * v + c[3][0] * iu * v,
        c[0][1] * iu * iv + c[1][1] * u * iv + c[2][1] * u * v + c[3][1] * iu * v,
        c[0][2] * iu * iv + c[1][2] * u * iv + c[2][2] * u * v + c[3][2] * iu * v,
    ]
}

/// Mesh描画: 制御グリッドをそのまま細分メッシュ化 (UVはグリッド正規位置)。
fn draw_mesh(
    painter: &egui::Painter,
    rect: egui::Rect,
    mesh: &MeshState,
    tex: Option<&egui::TextureHandle>,
    opacity: f32,
) {
    let to_screen = |p: [f32; 2]| {
        egui::Pos2::new(
            rect.min.x + p[0] * rect.width(),
            rect.min.y + p[1] * rect.height(),
        )
    };
    let cols = mesh.cols as usize;
    let rows = mesh.rows as usize;
    let get = |i: usize, j: usize| to_screen(mesh.points[mesh.idx(i as u32, j as u32)]);
    match tex {
        Some(h) => {
            let mut m = egui::Mesh::with_texture(h.id());
            let tint = egui::Color32::from_rgba_unmultiplied(
                255,
                255,
                255,
                (opacity.clamp(0.0, 1.0) * 255.0) as u8,
            );
            for j in 0..=rows {
                for i in 0..=cols {
                    m.vertices.push(egui::epaint::Vertex {
                        pos: get(i, j),
                        uv: egui::Pos2::new(i as f32 / cols as f32, j as f32 / rows as f32),
                        color: tint,
                    });
                }
            }
            let at = |i: usize, j: usize| (j * (cols + 1) + i) as u32;
            for j in 0..rows {
                for i in 0..cols {
                    m.indices
                        .extend_from_slice(&[at(i, j), at(i + 1, j), at(i + 1, j + 1)]);
                    m.indices
                        .extend_from_slice(&[at(i, j), at(i + 1, j + 1), at(i, j + 1)]);
                }
            }
            painter.add(egui::Shape::mesh(m));
        }
        None => {
            // 未読込時は位置グラデーションで形状確認
            let k = opacity.clamp(0.0, 1.0);
            let mut m = egui::Mesh::default();
            for j in 0..=rows {
                for i in 0..=cols {
                    let c = egui::Color32::from_rgb(
                        (i as f32 / cols as f32 * 255.0 * k) as u8,
                        (j as f32 / rows as f32 * 255.0 * k) as u8,
                        (160.0 * k) as u8,
                    );
                    m.colored_vertex(get(i, j), c);
                }
            }
            let at = |i: usize, j: usize| (j * (cols + 1) + i) as u32;
            for j in 0..rows {
                for i in 0..cols {
                    m.add_triangle(at(i, j), at(i + 1, j), at(i + 1, j + 1));
                    m.add_triangle(at(i, j), at(i + 1, j + 1), at(i, j + 1));
                }
            }
            painter.add(egui::Shape::mesh(m));
        }
    }
    // 外周アウトライン
    let mut border = Vec::new();
    for i in 0..=cols {
        border.push(get(i, 0));
    }
    for j in 1..=rows {
        border.push(get(cols, j));
    }
    for i in (0..cols).rev() {
        border.push(get(i, rows));
    }
    for j in (1..rows).rev() {
        border.push(get(0, j));
    }
    painter.add(egui::Shape::closed_line(
        border,
        egui::Stroke::new(1.5_f32, egui::Color32::WHITE),
    ));
}

/// 入力トリミング: 正規化[x,y,w,h]をpixel crop。全面ならclone回避。
fn apply_crop(base: &image::RgbaImage, crop: [f32; 4]) -> image::RgbaImage {
    let (bw, bh) = (base.width() as f32, base.height() as f32);
    if crop[2] >= 0.999 && crop[3] >= 0.999 && crop[0] <= 0.001 && crop[1] <= 0.001 {
        return base.clone();
    }
    let x = (crop[0] * bw).clamp(0.0, bw - 2.0) as u32;
    let y = (crop[1] * bh).clamp(0.0, bh - 2.0) as u32;
    let w = (crop[2] * bw).clamp(2.0, bw - x as f32) as u32;
    let h = (crop[3] * bh).clamp(2.0, bh - y as f32) as u32;
    image::imageops::crop_imm(base, x, y, w, h).to_image()
}

/// 出力段: ウィンドウ内に選択解像度のアスペクトで収まるキャンバス rect を返す。
fn aspect_fit(full: egui::Rect, w: u32, h: u32) -> egui::Rect {
    let (w, h) = (w.max(1) as f32, h.max(1) as f32);
    let scale = (full.width() / w).min(full.height() / h);
    let size = egui::Vec2::new(w * scale, h * scale);
    egui::Rect::from_center_size(full.center(), size)
}

/// 出力段: キャンバス内の面配置rect (正規化→pixel)。
fn surface_rect(canvas: egui::Rect, r: [f32; 4]) -> egui::Rect {
    egui::Rect::from_min_size(
        egui::Pos2::new(
            canvas.min.x + r[0] * canvas.width(),
            canvas.min.y + r[1] * canvas.height(),
        ),
        egui::Vec2::new(r[2] * canvas.width(), r[3] * canvas.height()),
    )
}

/// モード別描画ディスパッチ (Maskはマスク済みtextureをQuad描画)。
fn draw_surface(
    painter: &egui::Painter,
    rect: egui::Rect,
    mode: SurfaceMode,
    quad: [[f32; 2]; 4],
    mesh: &MeshState,
    tex: Option<&egui::TextureHandle>,
    opacity: f32,
) {
    match mode {
        SurfaceMode::Quad | SurfaceMode::Mask => draw_quad(painter, rect, quad, tex, opacity),
        SurfaceMode::Mesh => draw_mesh(painter, rect, mesh, tex, opacity),
    }
}

/// 編集グリッド描画 (10分割+中央強調)。
fn draw_grid(painter: &egui::Painter, rect: egui::Rect) {
    let faint = egui::Stroke::new(
        1.0_f32,
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 24),
    );
    let strong = egui::Stroke::new(
        1.0_f32,
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 70),
    );
    for i in 0..=10 {
        let f = i as f32 / 10.0;
        let x = rect.min.x + f * rect.width();
        let y = rect.min.y + f * rect.height();
        let s = if i == 5 { strong } else { faint };
        painter.line_segment(
            [
                egui::Pos2::new(x, rect.min.y),
                egui::Pos2::new(x, rect.max.y),
            ],
            s,
        );
        painter.line_segment(
            [
                egui::Pos2::new(rect.min.x, y),
                egui::Pos2::new(rect.max.x, y),
            ],
            s,
        );
    }
}

/// スナップ: 0/0.5/1に寄せる (編集精度用)。
fn snap_axis(v: f32) -> f32 {
    for s in [0.0, 0.5, 1.0] {
        if (v - s).abs() < 0.015 {
            return s;
        }
    }
    v
}

impl eframe::App for MapperApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // --- FPS平滑化 (EMA)。瞬時値はブレが大きいので指数平均で表示 ---
        let dt = ctx.input(|i| i.stable_dt).max(1e-6);
        let inst = 1.0 / dt;
        // 初回は即座に合わせる
        if self.fps_ema <= 0.0 {
            self.fps_ema = inst;
        } else {
            self.fps_ema += (inst - self.fps_ema) * 0.06;
        }

        // Rキーでリセット (SPEC §6.1 MVP操作)
        if ctx.input(|i| i.key_pressed(egui::Key::R)) {
            self.reset_quad();
            self.push_history();
        }
        // Undo/Redo・出力系ショートカット
        if ctx.input_mut(|i| {
            i.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND,
                egui::Key::Z,
            ))
        }) {
            if ctx.input(|i| i.modifiers.shift) {
                self.redo();
            } else {
                self.undo();
            }
        }
        if ctx.input_mut(|i| {
            i.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND,
                egui::Key::Y,
            ))
        }) {
            self.redo();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::B)) {
            self.blackout = !self.blackout;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::P)) {
            self.freeze = !self.freeze;
        }
        // 数字キー1-9で面選択
        for (n, key) in [
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
            egui::Key::Num6,
            egui::Key::Num7,
            egui::Key::Num8,
            egui::Key::Num9,
        ]
        .iter()
        .enumerate()
        {
            if ctx.input(|i| i.key_pressed(*key)) && n < self.surfaces.len() {
                self.active = n;
                self.dragging = None;
                self.selected = None;
            }
        }
        // Fで出力フルスクリーン切替 (開いている時のみ)
        if ctx.input(|i| i.key_pressed(egui::Key::F)) {
            self.output_fullscreen = !self.output_fullscreen;
            if self.show_output.load(Ordering::Relaxed) {
                ctx.send_viewport_cmd_to(
                    egui::ViewportId::from_hash_of("freemapper-output"),
                    egui::ViewportCommand::Fullscreen(self.output_fullscreen),
                );
            }
        }
        // Cmd/Ctrl+Sで上書き保存 (パス無しはダイアログ)
        if ctx.input_mut(|i| {
            i.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND,
                egui::Key::S,
            ))
        }) {
            if self.save_path.is_some() {
                let path = self.save_path.clone().unwrap();
                let data = self.collect_save();
                match persist::save_to(&path, &data) {
                    Ok(()) => {
                        self.save_message = Some(format!(
                            "{}: {}",
                            self.i18n.t("preset.saved"),
                            path.to_string_lossy()
                        ));
                    }
                    Err(e) => {
                        self.save_message = Some(format!("{}: {e}", self.i18n.t("preset.error")));
                    }
                }
            } else {
                self.do_save();
            }
        }

        // フリーズ中はtexture更新停止 (最終フレーム保持)
        if !self.freeze {
            self.update_texture(ctx);
        }
        // 動画速度の確定適用 (スライダー操作終了でspawnし直し)
        if self.speed_pending && ctx.input(|i| i.pointer.any_released()) {
            self.speed_pending = false;
            self.respawn_video();
        }
        // 自動バックアップ (90秒毎・クラッシュ時の復旧用)
        if self.last_autosave.elapsed().as_secs() >= 90 {
            self.last_autosave = Instant::now();
            let path = match &self.save_path {
                Some(p) => p.with_file_name(format!(
                    "{}_autosave.json",
                    p.file_stem().and_then(|s| s.to_str()).unwrap_or("preset")
                )),
                None => PathBuf::from("autosave.freemapper.json"),
            };
            let data = self.collect_save();
            match persist::save_to(&path, &data) {
                Ok(()) => {
                    self.autosave_count += 1;
                    self.autosave_label = format!(
                        "{} #{}: {}",
                        self.i18n.t("preset.autosaved"),
                        self.autosave_count,
                        path.to_string_lossy()
                    );
                }
                Err(e) => {
                    self.autosave_label = format!("autosave: {e}");
                }
            }
        }

        // --- OSC適用 (受信スレッド→UIスレッド) ---
        for cmd in self.osc.drain() {
            match cmd {
                OscCmd::Brightness(v) => {
                    self.effects.brightness = (v as i32).clamp(-100, 100);
                    self.fx_dirty = true;
                }
                OscCmd::Contrast(v) => {
                    self.effects.contrast = v.clamp(-100.0, 100.0);
                    self.fx_dirty = true;
                }
                OscCmd::Saturation(v) => {
                    self.effects.saturation = v.clamp(0.0, 200.0);
                    self.fx_dirty = true;
                }
                OscCmd::Hue(v) => {
                    self.effects.hue_shift = (v as i32).clamp(-180, 180);
                    self.fx_dirty = true;
                }
                OscCmd::Blur(v) => {
                    self.effects.blur_sigma = v.clamp(0.0, 10.0);
                    self.fx_dirty = true;
                }
                OscCmd::Generator(i) => {
                    // アクティブ面のスロットがジェネレーターなら種別変更、
                    // 違えばジェネレータースロットに割当変更
                    let all = GeneratorKind::all();
                    let g = all[i % all.len()];
                    let is_gen = !self.active_slot().is_file();
                    if is_gen {
                        self.set_active_gen(g);
                    } else {
                        let sid = self.ensure_generator_slot();
                        if let SlotKind::Generator(gg) =
                            &mut self.slots.iter_mut().find(|s| s.id == sid).unwrap().kind
                        {
                            *gg = g;
                        }
                        self.active_surface_mut().source_id = sid;
                        self.fx_dirty = true;
                    }
                }
                OscCmd::Surface(i) => {
                    if !self.surfaces.is_empty() {
                        self.active = i.min(self.surfaces.len() - 1);
                        self.dragging = None;
                    }
                }
                OscCmd::Fullscreen(b) => {
                    self.output_fullscreen = b;
                    if self.show_output.load(Ordering::Relaxed) {
                        ctx.send_viewport_cmd_to(
                            egui::ViewportId::from_hash_of("freemapper-output"),
                            egui::ViewportCommand::Fullscreen(b),
                        );
                    }
                }
            }
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            // 上=タイトルバー (言語切替は左「設定」に移動)
            ui.label(format!(
                "{} — {}",
                self.i18n.t("app.title"),
                self.i18n.t("app.subtitle")
            ));
        });

        egui::SidePanel::left("sources").show(ctx, |ui| {
            // --- 左=ソーススロット (面はここから割当) ---
            ui.heading(self.i18n.t("source.label"));
            let active_sid = self.active_surface().source_id.clone();
            let mut assign_to: Option<String> = None;
            let mut remove_sid: Option<String> = None;
            for s in &self.slots {
                let mark = if s.id == active_sid { " ●" } else { "" };
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(false, format!("{}{}", s.display_label(), mark))
                        .clicked()
                    {
                        assign_to = Some(s.id.clone());
                    }
                    if s.id == active_sid && self.slots.len() > 1 && ui.small_button("−").clicked()
                    {
                        remove_sid = Some(s.id.clone());
                    }
                });
            }
            if let Some(sid) = assign_to {
                self.active_surface_mut().source_id = sid;
                self.fx_dirty = true;
                self.sync_to_project();
                self.push_history();
            }
            if let Some(sid) = remove_sid {
                self.remove_slot(&sid);
            }
            ui.horizontal(|ui| {
                if ui.button(self.i18n.t("slot.add_gen")).clicked() {
                    self.add_generator_slot();
                }
                if ui.button(self.i18n.t("slot.add_file")).clicked() {
                    self.open_file_dialog();
                }
            });
            // --- アクティブ面のスロット編集 (入力処理) ---
            {
                let kind_is_gen = !self.active_slot().is_file();
                if kind_is_gen {
                    let cur = match &self.active_slot().kind {
                        SlotKind::Generator(g) => *g,
                        _ => GeneratorKind::Waves,
                    };
                    ui.label(format!(
                        "{}: {}",
                        self.i18n.t("gen.label"),
                        self.active_slot().id
                    ));
                    let t_cur = self.i18n.t(cur.label_key()).to_string();
                    let mut picked: Option<GeneratorKind> = None;
                    egui::ComboBox::from_id_salt("gen-select")
                        .selected_text(t_cur)
                        .show_ui(ui, |ui| {
                            for g in GeneratorKind::all() {
                                let t = self.i18n.t(g.label_key()).to_string();
                                if ui.selectable_label(cur == g, t).clicked() {
                                    picked = Some(g);
                                }
                            }
                        });
                    if let Some(g) = picked {
                        self.set_active_gen(g);
                    }
                } else {
                    let (nm, paused, animated) = {
                        let slot = self.active_slot();
                        let paused = match &slot.kind {
                            SlotKind::File {
                                media: Media::Video { stream, .. },
                                ..
                            } => stream.paused,
                            _ => false,
                        };
                        let animated = slot.is_animated_file();
                        (slot.display_label(), paused, animated)
                    };
                    ui.monospace(nm);
                    if animated && !paused {
                        // GIF/動画: 一時停止・速度・ループ
                        let t_pause = self.i18n.t("media.pause").to_string();
                        let t_play = self.i18n.t("media.play").to_string();
                        if ui.button(if paused { t_play } else { t_pause }).clicked() {
                            self.set_active_paused(!paused);
                        }
                        let mut sp = self.active_slot().speed;
                        if ui
                            .add(
                                egui::Slider::new(&mut sp, 0.25..=2.0)
                                    .text(self.i18n.t("media.speed")),
                            )
                            .changed()
                        {
                            self.set_active_speed(sp);
                        }
                        let mut lp = self.active_slot().repeat;
                        if ui.checkbox(&mut lp, self.i18n.t("media.loop")).changed() {
                            self.set_active_repeat(lp);
                        }
                    } else if animated {
                        let t_play = self.i18n.t("media.play").to_string();
                        if ui.button(t_play).clicked() {
                            self.set_active_paused(false);
                        }
                    }
                }
            }
            if let Some(e) = &self.media_error {
                ui.colored_label(egui::Color32::from_rgb(255, 120, 120), e);
            }
            // 入力段: トリミング (アクティブスロットの切出し範囲)
            {
                let t_crop = self.i18n.t("src.crop").to_string();
                ui.collapsing(t_crop, |ui| {
                    let mut c = self.active_slot().crop;
                    let mut changed = false;
                    for (i, key) in ["crop.x", "crop.y", "crop.w", "crop.h"].iter().enumerate() {
                        let t = self.i18n.t(key).to_string();
                        changed |= ui
                            .add(egui::Slider::new(&mut c[i], 0.0..=1.0).text(t))
                            .changed();
                    }
                    if changed {
                        c[2] = c[2].max(0.02);
                        c[3] = c[3].max(0.02);
                        self.active_slot_mut().crop = c;
                        self.fx_dirty = true;
                    }
                });
            }
            ui.label(self.i18n.t("source.network"));

            ui.separator();
            ui.heading(self.i18n.t("fx.label"));
            let fx = &mut self.effects;
            let mut changed = false;
            changed |= ui
                .add(
                    egui::Slider::new(&mut fx.brightness, -100..=100)
                        .text(self.i18n.t("fx.brightness")),
                )
                .changed();
            changed |= ui
                .add(
                    egui::Slider::new(&mut fx.contrast, -100.0..=100.0)
                        .text(self.i18n.t("fx.contrast")),
                )
                .changed();
            changed |= ui
                .add(
                    egui::Slider::new(&mut fx.saturation, 0.0..=200.0)
                        .text(self.i18n.t("fx.saturation")),
                )
                .changed();
            changed |= ui
                .add(egui::Slider::new(&mut fx.hue_shift, -180..=180).text(self.i18n.t("fx.hue")))
                .changed();
            changed |= ui
                .checkbox(&mut fx.grayscale, self.i18n.t("fx.gray"))
                .changed();
            changed |= ui
                .checkbox(&mut fx.invert, self.i18n.t("fx.invert"))
                .changed();
            changed |= ui
                .add(egui::Slider::new(&mut fx.blur_sigma, 0.0..=10.0).text(self.i18n.t("fx.blur")))
                .changed();
            if changed {
                self.fx_dirty = true;
            }
            if ui.button(self.i18n.t("fx.reset")).clicked() {
                self.effects.reset();
                self.fx_dirty = true;
            }

            ui.separator();
            // --- 左=サーフェス(シーン)一覧: 選択 / 追加 / 削除 ---
            ui.heading(self.i18n.t("surf.label"));
            let mut select_to: Option<usize> = None;
            let mut do_add = false;
            let mut do_remove = false;
            ui.horizontal_wrapped(|ui| {
                for (i, s) in self.surfaces.iter().enumerate() {
                    let mark = if s.visible { "" } else { " (off)" };
                    if ui
                        .selectable_label(self.active == i, format!("{}{}", s.name, mark))
                        .clicked()
                    {
                        select_to = Some(i);
                    }
                }
                do_add = ui.button(self.i18n.t("surf.add_s")).clicked();
                do_remove = ui.button(self.i18n.t("surf.remove_s")).clicked();
            });
            if let Some(i) = select_to {
                self.active = i;
                self.dragging = None;
                self.selected = None;
            }
            if do_add {
                self.add_surface();
            }
            if do_remove {
                self.remove_surface();
            }
            // 重ね順・複製・配置
            ui.horizontal_wrapped(|ui| {
                if ui.button(self.i18n.t("surf.forward")).clicked() {
                    self.surface_forward();
                }
                if ui.button(self.i18n.t("surf.backward")).clicked() {
                    self.surface_backward();
                }
                if ui.button(self.i18n.t("surf.duplicate")).clicked() {
                    self.duplicate_surface();
                }
                if ui.button(self.i18n.t("surf.center")).clicked() {
                    self.active_surface_mut().center_shape();
                    self.sync_to_project();
                    self.push_history();
                }
                if ui.button(self.i18n.t("surf.fill")).clicked() {
                    self.active_surface_mut().fill_shape();
                    self.sync_to_project();
                    self.push_history();
                }
            });

            ui.separator();
            ui.heading(self.i18n.t("preset.label"));
            ui.horizontal(|ui| {
                if ui.button(self.i18n.t("preset.save")).clicked() {
                    self.do_save();
                }
                if ui.button(self.i18n.t("preset.load")).clicked() {
                    self.do_load();
                }
            });
            if let Some(p) = &self.save_path {
                ui.monospace(p.to_string_lossy().to_string());
            }
            if let Some(msg) = &self.save_message {
                ui.label(msg);
            }
            if !self.autosave_label.is_empty() {
                ui.weak(self.autosave_label.clone());
            }

            ui.separator();
            // --- 左=設定 ---
            ui.heading(self.i18n.t("settings.label"));
            ui.horizontal(|ui| {
                ui.label(self.i18n.t("lang.label"));
                if ui
                    .selectable_label(self.i18n.lang == Lang::Ja, "日本語")
                    .clicked()
                {
                    self.i18n.lang = Lang::Ja;
                }
                if ui
                    .selectable_label(self.i18n.lang == Lang::En, "English")
                    .clicked()
                {
                    self.i18n.lang = Lang::En;
                }
            });
            // OSC受信 (TouchOSC等から操作)
            {
                let mut on = self.osc_enabled;
                let mut port = self.osc_port as i32;
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut on, self.i18n.t("osc.label")).changed() {
                        self.osc_enabled = on;
                        if on {
                            self.osc.start(self.osc_port);
                        } else {
                            self.osc.stop();
                        }
                    }
                    if ui
                        .add(egui::DragValue::new(&mut port).range(1024..=65535))
                        .changed()
                    {
                        self.osc_port = port.clamp(1024, 65535) as u16;
                        if self.osc_enabled {
                            self.osc.start(self.osc_port); // ポート変更=再bind
                        }
                    }
                });
                if self.osc.running {
                    ui.monospace(format!(
                        "{}:{} ({} msgs) {}",
                        self.osc.port,
                        self.i18n.t("osc.listening"),
                        self.osc.received,
                        self.osc.last_addr
                    ));
                } else if !self.osc.detail.is_empty() {
                    ui.colored_label(egui::Color32::from_rgb(255, 120, 120), &self.osc.detail);
                }
                ui.label(self.i18n.t("osc.hint"));
            }
        });

        // --- 右=出力段の制御 (解像度・配置・送出) ---
        egui::SidePanel::right("output").show(ctx, |ui| {
            ui.heading(self.i18n.t("out.label"));
            ui.label(self.i18n.t("out.canvas"));
            // プリセット + カスタム入力 (プリセット以外も入力可能)
            ui.horizontal(|ui| {
                let res_label = format!("{}×{}", self.out_w, self.out_h);
                egui::ComboBox::from_id_salt("out-res")
                    .selected_text(res_label)
                    .show_ui(ui, |ui| {
                        for (w, h) in [(1280, 720), (1920, 1080), (2560, 1440), (3840, 2160)] {
                            if ui
                                .selectable_label(
                                    self.out_w == w && self.out_h == h,
                                    format!("{w}×{h}"),
                                )
                                .clicked()
                            {
                                self.out_w = w;
                                self.out_h = h;
                            }
                        }
                    });
                let mut w = self.out_w as i32;
                let mut h = self.out_h as i32;
                if ui
                    .add(egui::DragValue::new(&mut w).range(320..=7680))
                    .changed()
                {
                    self.out_w = w.clamp(320, 7680) as u32;
                }
                if ui
                    .add(egui::DragValue::new(&mut h).range(240..=4320))
                    .changed()
                {
                    self.out_h = h.clamp(240, 4320) as u32;
                }
            });
            ui.monospace(format!(
                "canvas: {}×{} ({} surfaces)",
                self.out_w,
                self.out_h,
                self.surfaces.iter().filter(|s| s.visible).count()
            ));
            // --- 出力キャンバス: 各面の配置を正アスペクトで決める ---
            ui.label(self.i18n.t("out.place"));
            {
                let avail_w = ui.available_width().max(120.0);
                let ch = avail_w * self.out_h as f32 / self.out_w.max(1) as f32;
                let (resp, painter) = ui
                    .allocate_painter(egui::Vec2::new(avail_w, ch), egui::Sense::click_and_drag());
                let canvas = resp.rect;
                painter.rect_filled(canvas, 2.0, egui::Color32::BLACK);
                painter.rect_stroke(
                    canvas,
                    2.0,
                    egui::Stroke::new(1.0_f32, egui::Color32::DARK_GRAY),
                    egui::StrokeKind::Outside,
                );
                // 全面rect + 名前
                for (i, s) in self.surfaces.iter().enumerate() {
                    if !s.visible {
                        continue;
                    }
                    let r = surface_rect(canvas, s.out_rect);
                    let col = if i == self.active {
                        egui::Color32::YELLOW
                    } else {
                        egui::Color32::from_gray(160)
                    };
                    painter.rect_stroke(
                        r,
                        0.0,
                        egui::Stroke::new(if i == self.active { 2.0_f32 } else { 1.0_f32 }, col),
                        egui::StrokeKind::Outside,
                    );
                    painter.text(
                        r.min + egui::Vec2::new(3.0, 0.0),
                        egui::Align2::LEFT_TOP,
                        &s.name,
                        egui::FontId::monospace(10.0),
                        col,
                    );
                }
                // アクティブ面のrectドラッグ移動
                if self.active < self.surfaces.len() {
                    let delta = ctx.input(|i| i.pointer.delta());
                    if resp.dragged() && delta != egui::Vec2::ZERO {
                        let r = self.active_surface().out_rect;
                        let nx = r[0] + delta.x / canvas.width();
                        let ny = r[1] + delta.y / canvas.height();
                        let fixed = persist::normalize_rect([nx, ny, r[2], r[3]]);
                        self.active_surface_mut().out_rect = fixed;
                        self.hist_pending = true;
                    }
                    if ctx.input(|i| i.pointer.any_released()) && self.hist_pending {
                        self.hist_pending = false;
                        self.sync_to_project();
                        self.push_history();
                    }
                }
            }
            // アクティブ面の配置数値 (pixel換算表示)
            {
                let mut r = self.active_surface().out_rect;
                let px = |f: f32, base: u32| (f * base as f32).round() as u32;
                let mut changed = false;
                ui.horizontal(|ui| {
                    changed |= ui
                        .add(egui::Slider::new(&mut r[0], 0.0..=1.0).text("X"))
                        .changed();
                    changed |= ui
                        .add(egui::Slider::new(&mut r[2], 0.05..=1.0).text("W"))
                        .changed();
                });
                ui.horizontal(|ui| {
                    changed |= ui
                        .add(egui::Slider::new(&mut r[1], 0.0..=1.0).text("Y"))
                        .changed();
                    changed |= ui
                        .add(egui::Slider::new(&mut r[3], 0.05..=1.0).text("H"))
                        .changed();
                });
                if changed {
                    self.active_surface_mut().out_rect = persist::normalize_rect(r);
                    self.sync_to_project();
                    self.hist_pending = true;
                }
                ui.monospace(format!(
                    "{}: {}×{} @ {},{}px",
                    self.active_surface().name,
                    px(r[2], self.out_w),
                    px(r[3], self.out_h),
                    px(r[0], self.out_w),
                    px(r[1], self.out_h)
                ));
                if ui.button(self.i18n.t("out.fill_canvas")).clicked() {
                    self.active_surface_mut().out_rect = [0.0, 0.0, 1.0, 1.0];
                    self.sync_to_project();
                    self.push_history();
                }
            }
            // 本番系トグル (B=blackout, P=freeze)
            ui.horizontal(|ui| {
                let mut bo = self.blackout;
                if ui.checkbox(&mut bo, self.i18n.t("out.blackout")).changed() {
                    self.blackout = bo;
                }
                let mut fr = self.freeze;
                if ui.checkbox(&mut fr, self.i18n.t("out.freeze")).changed() {
                    self.freeze = fr;
                }
            });
            ui.separator();
            let out_open = self.show_output.load(Ordering::Relaxed);
            if ui
                .button(if out_open {
                    self.i18n.t("out.close")
                } else {
                    self.i18n.t("out.open")
                })
                .clicked()
            {
                self.show_output.store(!out_open, Ordering::Relaxed);
            }
            if ui
                .checkbox(&mut self.output_fullscreen, self.i18n.t("out.fullscreen"))
                .changed()
                && self.show_output.load(Ordering::Relaxed)
            {
                ctx.send_viewport_cmd_to(
                    egui::ViewportId::from_hash_of("freemapper-output"),
                    egui::ViewportCommand::Fullscreen(self.output_fullscreen),
                );
            }
            ui.label(self.i18n.t("out.hint"));
            // Syphon出力 (macOSのみ有効・他OSはスタブ表示)
            {
                let mut on = self.syphon.enabled;
                if ui.checkbox(&mut on, self.i18n.t("syphon.label")).changed() {
                    self.syphon.set_enabled(on);
                }
                let (key, color) = match self.syphon.status {
                    SyphonStatus::Off => ("syphon.off", egui::Color32::GRAY),
                    SyphonStatus::Waiting => ("syphon.waiting", egui::Color32::YELLOW),
                    SyphonStatus::Live => ("syphon.live", egui::Color32::GREEN),
                    SyphonStatus::Error => ("syphon.error", egui::Color32::RED),
                };
                ui.colored_label(color, self.i18n.t(key));
                if !self.syphon.detail.is_empty() {
                    ui.monospace(self.syphon.detail.clone());
                }
            }
            ui.separator();
            // 平滑化FPS + フレームms併記
            ui.label(format!(
                "{}: {:.0} ({:.1} ms)",
                self.i18n.t("output.fps"),
                self.fps_ema,
                1000.0 / self.fps_ema.max(0.01)
            ));
            ui.separator();
            ui.collapsing("Stream schemes (SPEC §6.4)", |ui| {
                for uri in [
                    "generator://waves",
                    "file://photo.jpg",
                    "shader://isf/default",
                    "rtsp://cam/stream",
                    "rtmp://server/live",
                    "srt://10.0.0.2:9000",
                    "ndi://source",
                ] {
                    let scheme = SourceScheme::from_uri(uri);
                    let badge = match scheme {
                        Some(s) if s.implemented_in_v01() => "v0.1 ✅",
                        Some(_) => "v0.2予定",
                        None => "?",
                    };
                    ui.monospace(format!("{uri}  [{badge}]"));
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            // 中央=選択中ソースの編集 (入力段の形状編集)
            self.active = self.active.min(self.surfaces.len().saturating_sub(1));
            ui.horizontal(|ui| {
                ui.heading(self.i18n.t("edit.label"));
                let mut nm = self.active_surface().name.clone();
                if ui.text_edit_singleline(&mut nm).changed() {
                    // 空・重複は許容 (idで区別)。20文字制限。
                    nm.truncate(20);
                    if !nm.is_empty() {
                        self.active_surface_mut().name = nm;
                        self.sync_to_project();
                    }
                }
            });
            // --- アクティブ面の形状モード ---
            let t_quad = self.i18n.t("surf.quad").to_string();
            let t_mesh = self.i18n.t("surf.mesh").to_string();
            let t_mask = self.i18n.t("surf.mask").to_string();
            let t_visible = self.i18n.t("surf.visible").to_string();
            let mut mode_switched = false;
            {
                let s = self.active_surface_mut();
                ui.horizontal(|ui| {
                    ui.checkbox(&mut s.visible, t_visible);
                    for (m, label) in [
                        (SurfaceMode::Quad, t_quad),
                        (SurfaceMode::Mesh, t_mesh),
                        (SurfaceMode::Mask, t_mask),
                    ] {
                        if ui.selectable_label(s.mode == m, label).clicked() {
                            s.mode = m;
                            mode_switched = true;
                        }
                    }
                });
            }
            if mode_switched {
                // Mask<->他切替でtexture作り直しが必要
                self.fx_dirty = true;
                self.sync_to_project();
                self.push_history();
                self.selected = None;
            }
            // アクティブ面へのソース割当 (出力段への入力指定)
            {
                let cur_sid = self.active_surface().source_id.clone();
                let cur_label = self
                    .slots
                    .iter()
                    .find(|s| s.id == cur_sid)
                    .map(|s| s.display_label())
                    .unwrap_or_default();
                let t_src = self.i18n.t("surf.source").to_string();
                let mut picked: Option<String> = None;
                ui.horizontal(|ui| {
                    ui.label(t_src);
                    egui::ComboBox::from_id_salt("surf-source")
                        .selected_text(cur_label)
                        .show_ui(ui, |ui| {
                            for s in &self.slots {
                                if ui
                                    .selectable_label(s.id == cur_sid, s.display_label())
                                    .clicked()
                                {
                                    picked = Some(s.id.clone());
                                }
                            }
                        });
                });
                if let Some(sid) = picked {
                    self.active_surface_mut().source_id = sid;
                    self.fx_dirty = true;
                    self.sync_to_project();
                    self.push_history();
                }
            }
            // 不透明度 (頂点alpha・GPU合成)
            {
                let mut op = (self.active_surface().opacity * 100.0).round();
                if ui
                    .add(egui::Slider::new(&mut op, 0.0..=100.0).text(self.i18n.t("surf.opacity")))
                    .changed()
                {
                    self.active_surface_mut().opacity = (op / 100.0).clamp(0.0, 1.0);
                    self.hist_pending = true;
                }
            }
            // ソフトエッジ (ブレンディング用・面別)
            let t_edge = self.i18n.t("surf.edge").to_string();
            ui.collapsing(t_edge, |ui| {
                let mut e = self.active_surface().edge;
                let mut changed = false;
                for (i, key) in ["edge.l", "edge.r", "edge.t", "edge.b"].iter().enumerate() {
                    changed |= ui
                        .add(egui::Slider::new(&mut e[i], 0.0..=0.5).text(self.i18n.t(key)))
                        .changed();
                }
                if changed {
                    self.active_surface_mut().edge = e;
                    self.fx_dirty = true;
                    self.hist_pending = true;
                }
            });
            // 選択点の数値入力 + スナップ/グリッド
            {
                let mut ctrl = self.active_surface().control_points();
                if let Some(idx) = self.selected {
                    if idx < ctrl.len() {
                        let mut x = ctrl[idx][0];
                        let mut y = ctrl[idx][1];
                        ui.horizontal(|ui| {
                            ui.label(format!("{}[{}]", self.i18n.t("edit.selected"), idx));
                            if ui
                                .add(egui::DragValue::new(&mut x).range(0.0..=1.0).speed(0.001))
                                .changed()
                                | ui.add(egui::DragValue::new(&mut y).range(0.0..=1.0).speed(0.001))
                                    .changed()
                            {
                                ctrl[idx] = [x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)];
                                self.active_surface_mut().apply_control_points(&ctrl);
                                self.sync_to_project();
                                self.hist_pending = true;
                            }
                        });
                    }
                }
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.snap_enabled, self.i18n.t("edit.snap"));
                    ui.checkbox(&mut self.show_grid, self.i18n.t("edit.grid"));
                });
            }
            let active_mode = self.active_surface().mode;
            match active_mode {
                SurfaceMode::Mesh => {
                    let (mut c, mut r) = {
                        let s = self.active_surface();
                        (s.mesh.cols, s.mesh.rows)
                    };
                    ui.horizontal(|ui| {
                        if ui
                            .add(egui::Slider::new(&mut c, 2..=16).text(self.i18n.t("surf.cols")))
                            .changed()
                            || ui
                                .add(
                                    egui::Slider::new(&mut r, 2..=16)
                                        .text(self.i18n.t("surf.rows")),
                                )
                                .changed()
                        {
                            self.active_surface_mut().mesh.ensure_size(c, r);
                            self.sync_to_project();
                            self.push_history();
                        }
                    });
                    ui.label(self.i18n.t("surf.mesh_note"));
                }
                SurfaceMode::Mask => {
                    let t_add = self.i18n.t("surf.add").to_string();
                    let t_remove = self.i18n.t("surf.remove").to_string();
                    let t_invert = self.i18n.t("surf.invert").to_string();
                    ui.horizontal(|ui| {
                        if ui.button(t_add).clicked() {
                            self.active_surface_mut().mask.add_point();
                            self.sync_to_project();
                            self.push_history();
                        }
                        if ui.button(t_remove).clicked() {
                            self.active_surface_mut().mask.remove_point();
                            self.sync_to_project();
                            self.push_history();
                        }
                        if ui
                            .checkbox(&mut self.active_surface_mut().mask.invert, t_invert)
                            .changed()
                        {
                            self.sync_to_project();
                            self.push_history();
                        }
                    });
                    ui.label(self.i18n.t("surf.feather_note"));
                }
                SurfaceMode::Quad => {}
            }

            let avail = ui.available_size();
            let side = avail.x.min(avail.y - 130.0).max(200.0);
            let (resp, painter) =
                ui.allocate_painter(egui::Vec2::new(side, side), egui::Sense::click_and_drag());
            let rect = resp.rect;

            painter.rect_filled(rect, 4.0, egui::Color32::from_gray(18));
            if self.show_grid {
                draw_grid(&painter, rect);
            }
            // 全可視サーフェスを重ね描き (アクティブ面の確認用)
            for (i, s) in self.surfaces.iter().enumerate() {
                if !s.visible {
                    continue;
                }
                draw_surface(
                    &painter,
                    rect,
                    s.mode,
                    s.quad,
                    &s.mesh,
                    self.textures.get(i).and_then(|t| t.as_ref()),
                    s.opacity,
                );
            }

            // アクティブ面の制御点を取得
            let mut ctrl = self.active_surface().control_points();
            let to_screen = |p: [f32; 2]| {
                egui::Pos2::new(
                    rect.min.x + p[0] * rect.width(),
                    rect.min.y + p[1] * rect.height(),
                )
            };
            let pts: Vec<egui::Pos2> = ctrl.iter().map(|p| to_screen(*p)).collect();

            // ハンドルのドラッグ処理 (アクティブ面のみ)
            // press: 候補記録 / release: ほぼ動かなければ選択確定、動けば履歴push
            let is_mesh = self.active_surface().mode == SurfaceMode::Mesh;
            let grab_radius = if is_mesh { 8.0 } else { 12.0 };
            if let Some(pos) = resp.interact_pointer_pos() {
                if ctx.input(|i| i.pointer.any_pressed()) {
                    let mut best: Option<usize> = None;
                    let mut best_d = grab_radius * 2.0;
                    for (i, p) in pts.iter().enumerate() {
                        let d = p.distance(pos);
                        if d < best_d {
                            best_d = d;
                            best = Some(i);
                        }
                    }
                    self.dragging = best;
                    self.press_candidate = best;
                    self.drag_moved = false;
                }
            }
            if ctx.input(|i| i.pointer.any_released()) {
                if self.dragging.is_some() {
                    if self.drag_moved {
                        self.push_history();
                    } else if let Some(c) = self.press_candidate {
                        // クリック=選択
                        self.selected = Some(c);
                    }
                }
                self.dragging = None;
                self.press_candidate = None;
                self.drag_moved = false;
                self.sync_to_project();
                // スライダー系の確定もここで履歴化
                if self.hist_pending {
                    self.hist_pending = false;
                    self.push_history();
                }
            }
            if let Some(idx) = self.dragging {
                if ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
                    self.drag_moved = true;
                }
                if let Some(pos) = ctx.pointer_latest_pos() {
                    let mut nx = ((pos.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
                    let mut ny = ((pos.y - rect.min.y) / rect.height()).clamp(0.0, 1.0);
                    if self.snap_enabled {
                        nx = snap_axis(nx);
                        ny = snap_axis(ny);
                    }
                    if idx < ctrl.len() {
                        ctrl[idx] = [nx, ny];
                    }
                }
            }
            // 書き戻し (アクティブ面)
            self.active_surface_mut().apply_control_points(&ctrl);
            // 矢印キーで選択点をナッジ
            if let Some(idx) = self.selected {
                let step = if ctx.input(|i| i.modifiers.shift) {
                    0.01
                } else {
                    0.002
                };
                let mut d = [0.0, 0.0];
                if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
                    d[0] -= step;
                }
                if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
                    d[0] += step;
                }
                if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                    d[1] -= step;
                }
                if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                    d[1] += step;
                }
                if d != [0.0, 0.0] {
                    let mut c2 = self.active_surface().control_points();
                    if idx < c2.len() {
                        c2[idx] = [
                            (c2[idx][0] + d[0]).clamp(0.0, 1.0),
                            (c2[idx][1] + d[1]).clamp(0.0, 1.0),
                        ];
                        self.active_surface_mut().apply_control_points(&c2);
                        self.sync_to_project();
                        self.push_history();
                    }
                }
            }
            let dot_r = if self.active_surface().mode == SurfaceMode::Mesh {
                5.0
            } else {
                7.0
            };
            for (i, p) in pts.iter().enumerate() {
                let dragging = self.dragging == Some(i);
                let col = if self.selected == Some(i) {
                    egui::Color32::YELLOW
                } else {
                    egui::Color32::from_rgb(220, 40, 40)
                };
                painter.circle_filled(*p, if dragging { dot_r + 2.0 } else { dot_r }, col);
                painter.circle_stroke(*p, dot_r, egui::Stroke::new(1.5_f32, egui::Color32::WHITE));
            }

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button(self.i18n.t("action.reset")).clicked() {
                    self.reset_quad();
                    self.push_history();
                }
                ui.label(self.i18n.t("surface.hint"));
            });
            ui.label(self.i18n.t("keys.hint"));
            ui.collapsing("project.json (SPEC §9)", |ui| {
                self.sync_to_project();
                let json = serde_json::to_string_pretty(&self.project).unwrap_or_default();
                ui.monospace(json);
            });
            ui.label(self.i18n.t("status.v01"));
            ctx.request_repaint();
        });

        // --- 出力ウィンドウ (別viewport)。ワープ反映の本番画面 ---
        if self.show_output.load(Ordering::Relaxed) {
            {
                let mut items = self.shared.items.lock().unwrap();
                items.clear();
                for (i, s) in self.surfaces.iter().enumerate() {
                    if !s.visible {
                        continue;
                    }
                    items.push(SharedItem {
                        mode: s.mode,
                        quad: s.quad,
                        mesh: s.mesh.clone(),
                        texture: self.textures.get(i).and_then(|t| t.clone()),
                        opacity: s.opacity,
                        out_rect: s.out_rect,
                    });
                }
            }
            let shared = self.shared.clone();
            let flag = self.show_output.clone();
            let fullscreen = self.output_fullscreen;
            let blackout = self.blackout;
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("freemapper-output"),
                egui::ViewportBuilder::default()
                    .with_title("FreeMapper Output")
                    .with_inner_size([960.0, 540.0])
                    .with_fullscreen(fullscreen),
                move |ctx, _| {
                    if ctx.input(|i| i.viewport().close_requested()) {
                        flag.store(false, Ordering::Relaxed);
                    }
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
                        .show(ctx, |ui| {
                            let full = ui.available_rect_before_wrap();
                            let painter = ui.painter().clone();
                            // 出力段: 選択解像度にaspect-fitしたキャンバスに正規化配置
                            let canvas = aspect_fit(full, self.out_w, self.out_h);
                            painter.rect_filled(full, 0.0, egui::Color32::BLACK);
                            painter.rect_stroke(
                                canvas,
                                0.0,
                                egui::Stroke::new(1.0_f32, egui::Color32::DARK_GRAY),
                                egui::StrokeKind::Outside,
                            );
                            let items = shared.items.lock().unwrap().clone();
                            if blackout {
                                ui.centered_and_justified(|ui| {
                                    ui.colored_label(egui::Color32::RED, "BLACKOUT (B)");
                                });
                            } else {
                                for it in &items {
                                    // 出力段: 各面はout_rect配置で描く
                                    let r = surface_rect(canvas, it.out_rect);
                                    draw_surface(
                                        &painter,
                                        r,
                                        it.mode,
                                        it.quad,
                                        &it.mesh,
                                        it.texture.as_ref(),
                                        it.opacity,
                                    );
                                }
                            }
                            ui.label(format!("{}×{}", self.out_w, self.out_h));
                        });
                    ctx.request_repaint();
                },
            );
        }
    }
}

/// SPEC §3: Noto Sans JP同梱。eguiデフォルトフォントにはCJKグリフが無いため、
/// 日本語を先頭に挿入して豆腐化を防ぐ (Proportional / Monospace両方)。
fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "noto-sans-jp".to_owned(),
        egui::FontData::from_static(include_bytes!("../../../assets/fonts/NotoSansJP.ttf")).into(),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(list) = fonts.families.get_mut(&family) {
            list.insert(0, "noto-sans-jp".to_owned());
        }
    }
    ctx.set_fonts(fonts);
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 750.0])
            .with_title("FreeMapper v0.1"),
        ..Default::default()
    };
    eframe::run_native(
        "freemapper",
        options,
        Box::new(|cc| {
            setup_fonts(&cc.egui_ctx);
            Ok(Box::new(MapperApp::default()))
        }),
    )
}
