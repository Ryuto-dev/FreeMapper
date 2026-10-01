//! ソーススロット (入力段)。
//! 面(Surface)はスロットを指す。複数ソース→複数面の割当を可能にする。
//! crop/speed/repeatはスロット毎 (入力処理はソース単位)。

use std::path::PathBuf;

use image::RgbaImage;

use crate::generators::GeneratorKind;
use crate::media::Media;

pub enum SlotKind {
    Generator(GeneratorKind),
    File { media: Media, path: Option<PathBuf> },
}

pub struct SourceSlot {
    pub id: String,
    pub name: String,
    pub kind: SlotKind,
    pub crop: [f32; 4],
    pub speed: f32,
    pub repeat: bool,
    last_token: u64,
}

impl SourceSlot {
    pub fn new_generator(id: u32) -> Self {
        Self {
            id: format!("slot{id}"),
            name: format!("Gen{id}"),
            kind: SlotKind::Generator(GeneratorKind::Waves),
            crop: [0.0, 0.0, 1.0, 1.0],
            speed: 1.0,
            repeat: true,
            last_token: 0,
        }
    }

    pub fn new_file(id: u32, media: Media, path: PathBuf) -> Self {
        let name = media.name().to_string();
        Self {
            id: format!("slot{id}"),
            name,
            kind: SlotKind::File {
                media,
                path: Some(path),
            },
            crop: [0.0, 0.0, 1.0, 1.0],
            speed: 1.0,
            repeat: true,
            last_token: u64::MAX,
        }
    }

    pub fn is_file(&self) -> bool {
        matches!(self.kind, SlotKind::File { .. })
    }

    pub fn is_video(&self) -> bool {
        matches!(
            self.kind,
            SlotKind::File {
                media: Media::Video { .. },
                ..
            }
        )
    }

    pub fn is_animated_file(&self) -> bool {
        matches!(
            self.kind,
            SlotKind::File {
                media: Media::Animated { .. } | Media::Video { .. },
                ..
            }
        )
    }

    pub fn display_label(&self) -> String {
        match &self.kind {
            SlotKind::Generator(g) => format!("{}:{:?}", self.name, g),
            SlotKind::File { media, .. } => media.name().to_string(),
        }
    }

    /// Video pipeのdrain (毎フレーム呼ぶ)。
    pub fn poll(&mut self) {
        if let SlotKind::File { media, .. } = &mut self.kind {
            media.poll_video();
        }
    }

    /// 現在のbase画像 (crop前)。
    pub fn base(&self, t: f32) -> Option<RgbaImage> {
        match &self.kind {
            SlotKind::Generator(g) => Some(g.render(t)),
            SlotKind::File { media, .. } => Some(media.current_base().clone()),
        }
    }

    /// File系の変化検出。Generatorは常にtrue (アニメーション)。
    pub fn changed(&mut self) -> bool {
        match &self.kind {
            SlotKind::Generator(_) => true,
            SlotKind::File { media, .. } => {
                let tok = media.frame_token() as u64;
                let c = tok != self.last_token;
                self.last_token = tok;
                c
            }
        }
    }

    pub fn set_speed(&mut self, sp: f32) {
        self.speed = sp.clamp(0.25, 2.0);
        if let SlotKind::File {
            media: Media::Animated { speed, .. },
            ..
        } = &mut self.kind
        {
            *speed = self.speed;
        }
    }

    pub fn set_repeat(&mut self, repeat: bool) {
        self.repeat = repeat;
        if let SlotKind::File {
            media: Media::Animated { repeat: r, .. },
            ..
        } = &mut self.kind
        {
            *r = repeat;
        }
    }

    /// 動画のspawnし直し (速度・ループ変更時)。成功時true。
    pub fn respawn_video(&mut self) -> bool {
        let path = match &self.kind {
            SlotKind::File { path: Some(p), .. } => p.clone(),
            _ => return false,
        };
        let stem = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("video")
            .to_string();
        match crate::media::load_video_speed(&path, &stem, self.speed, self.repeat) {
            Ok(Media::Video { stream, .. }) => {
                if let SlotKind::File { media, .. } = &mut self.kind {
                    *media = Media::Video {
                        name: stream_name(&stem, &stream),
                        stream,
                    };
                    true
                } else {
                    false
                }
            }
            Ok(_) => false,
            Err(_) => false,
        }
    }
}

fn stream_name(stem: &str, stream: &crate::media::VideoStream) -> String {
    format!(
        "{stem} ({}×{} {}fps loop)",
        stream.w, stream.h, stream.fps_label
    )
}
