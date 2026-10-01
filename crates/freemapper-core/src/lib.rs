//! FreeMapper core v0.1 spike.
//! SPEC v0.1 §5: Input/Outputを Stream として抽象化。ファイルもネットワークも同じtrait。
//! SPEC v0.1 §6.1: Quad/Mesh/Mask の型だけ先に切る。実装はQuadから。

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

// ---------- Project schema ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub schema: u32,
    pub surfaces: Vec<Surface>,
    pub layers: Vec<Layer>,
}

impl Default for Project {
    fn default() -> Self {
        let quad = Surface {
            id: "s1".into(),
            kind: SurfaceKind::Quad(Quad {
                // 正規化座標系 (0..1)。レンダラー側で出力解像度に展開する。
                points: [[0.1, 0.1], [0.9, 0.1], [0.9, 0.9], [0.1, 0.9]],
            }),
        };
        Self {
            schema: SCHEMA_VERSION,
            surfaces: vec![quad],
            layers: vec![Layer {
                id: "l1".into(),
                source: SourceUri("generator://test-pattern".into()),
                surface: "s1".into(),
                opacity: 1.0,
            }],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Surface {
    pub id: String,
    pub kind: SurfaceKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SurfaceKind {
    Quad(Quad),
    /// v0.2以降用に型だけ用意。MVPレンダラーはQuadのみ描画。
    Mesh(Mesh),
    Mask(Mask),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quad {
    /// [TL, TR, BR, BL] の順。正規化座標。
    pub points: [[f32; 2]; 4],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mesh {
    pub cols: u32,
    pub rows: u32,
    pub points: Vec<[f32; 2]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mask {
    pub polygon: Vec<[f32; 2]>,
    pub feather: f32,
    pub invert: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layer {
    pub id: String,
    pub source: SourceUri,
    pub surface: String,
    pub opacity: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceUri(pub String);

// ---------- Stream I/O abstraction (SPEC §6.4) ----------

/// 全入力の共通trait。ファイルも RTSP/SRT も同じ扱いにするのがミソ。
/// v0.1は Generator (テストパターン) のみ実装。File/RTSP/SRTは v0.2で追加。
pub trait VideoSource: Send {
    fn uri_scheme(&self) -> &'static str;
    fn open(&mut self) {}
    fn close(&mut self) {}
}

/// 全出力の共通trait。Display も RTMP Push も同じ扱い。
pub trait VideoSink: Send {
    fn sink_kind(&self) -> &'static str;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceScheme {
    File,
    Capture,
    Generator,
    Rtsp,
    Rtmp,
    Srt,
    Ndi,
    Syphon,
    Spout,
    Shader,
}

impl SourceScheme {
    pub fn from_uri(uri: &str) -> Option<Self> {
        let scheme = uri.split("://").next()?;
        Some(match scheme {
            "file" => Self::File,
            "capture" => Self::Capture,
            "generator" => Self::Generator,
            "rtsp" => Self::Rtsp,
            "rtmp" => Self::Rtmp,
            "srt" => Self::Srt,
            "ndi" => Self::Ndi,
            "syphon" => Self::Syphon,
            "spout" => Self::Spout,
            "shader" => Self::Shader,
            _ => return None,
        })
    }

    /// v0.1で実装済みか。未実装はUIで「v0.2予定」と表示する。
    pub fn implemented_in_v01(self) -> bool {
        matches!(self, Self::File | Self::Generator | Self::Shader)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_project_roundtrips() {
        let p = Project::default();
        let json = serde_json::to_string(&p).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.schema, SCHEMA_VERSION);
        assert_eq!(back.surfaces.len(), 1);
    }

    #[test]
    fn scheme_parsing() {
        assert_eq!(
            SourceScheme::from_uri("srt://10.0.0.2:9000"),
            Some(SourceScheme::Srt)
        );
        assert_eq!(
            SourceScheme::from_uri("rtsp://cam/stream"),
            Some(SourceScheme::Rtsp)
        );
        assert!(SourceScheme::from_uri("foo://x").is_none());
    }
}
