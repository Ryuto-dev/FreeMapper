//! メディア読込 (v0.1)。
//! - 静止画: png/jpg/bmp/webp (image crate)
//! - GIFアニメ (フレーム展開→タイマ再生)
//! - 動画: mp4/mov等 (ffmpeg-sidecar経由で別プロセスdecode→rawvideo pipe→loop)

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use image::{AnimationDecoder, RgbaImage};

pub const MAX_GIF_DIM: u32 = 960;
pub const MAX_GIF_FRAMES: usize = 120;
/// 動画デコードの出力上限 (長い辺)。CPU負荷対策。
pub const MAX_VIDEO_SIDE: u32 = 1280;

#[derive(Debug)]
pub enum Media {
    Still {
        name: String,
        base: RgbaImage,
    },
    Animated {
        name: String,
        frames: Vec<RgbaImage>,
        /// 各フレームの表示時間
        delays: Vec<Duration>,
        started: Instant,
        /// 再生速度 (1.0=等速)。UIから変更。
        speed: f32,
        /// false=ワンショット (終端で停止)
        repeat: bool,
    },
    Video {
        name: String,
        stream: VideoStream,
    },
}

impl Media {
    pub fn name(&self) -> &str {
        match self {
            Self::Still { name, .. } => name,
            Self::Animated { name, .. } => name,
            Self::Video { name, .. } => name,
        }
    }

    /// 現在表示すべきフレームのbase画像
    pub fn current_base(&self) -> &RgbaImage {
        match self {
            Self::Still { base, .. } => base,
            Self::Animated { frames, .. } => {
                let idx = self.frame_token();
                &frames[idx.min(frames.len().saturating_sub(1))]
            }
            Self::Video { stream, .. } => &stream.last,
        }
    }

    /// アニメの現在フレーム番号 (Stillは常に0)。再アップロード判定用。
    /// Videoはdrain副作用あり (&mutが必要なため内部可変ではなく&mut self)。
    pub fn frame_token(&self) -> usize {
        match self {
            Self::Video { stream, .. } => stream.frame_no as usize,
            _ => self.current_index(),
        }
    }

    /// Video用: pipe到着分をdrain→最新化。変化時のみtrue。
    /// paused時はdrainしない (pipe背圧でffmpeg側が自然停止)。
    pub fn poll_video(&mut self) -> bool {
        match self {
            Self::Video { stream, .. } => stream.poll(),
            _ => false,
        }
    }

    fn current_index(&self) -> usize {
        match self {
            Self::Still { .. } => 0,
            Self::Video { .. } => 0, // Videoはframe_token側で処理
            Self::Animated {
                frames,
                delays,
                started,
                speed,
                repeat,
                ..
            } => {
                if frames.is_empty() {
                    return 0;
                }
                let total_ms = delays.iter().map(|d| d.as_millis()).sum::<u128>();
                if total_ms == 0 {
                    return 0;
                }
                // 速度適用: 経過時間をspeed倍して読む
                let scaled_ms = (started.elapsed().as_millis() as f32 * speed.max(0.05)) as u128;
                if !repeat && scaled_ms >= total_ms {
                    return frames.len() - 1; // ワンショット終端で停止
                }
                let mut t = (scaled_ms % total_ms) as u64;
                for (i, d) in delays.iter().enumerate() {
                    let ms = d.as_millis() as u64;
                    if t < ms {
                        return i;
                    }
                    t -= ms;
                }
                0
            }
        }
    }
}

/// 拡張子で振り分け読込。未対応(mp4等)はErr(メッセージはUI表示用)。
pub fn load_file(path: &Path) -> Result<Media, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("media")
        .to_string();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "bmp" | "webp" | "tif" | "tiff" => {
            let img = image::open(path)
                .map_err(|e| format!("open failed: {e}"))?
                .to_rgba8();
            Ok(Media::Still {
                name,
                base: fit_down(img, 1920),
            })
        }
        "gif" => load_gif(path, &name),
        "mp4" | "mov" | "m4v" | "avi" | "mkv" | "webm" | "hap" => {
            load_video_speed(path, &name, 1.0, true)
        }
        _ => Err(format!("{name}: 未対応の拡張子 .{ext}")),
    }
}

fn load_gif(path: &Path, name: &str) -> Result<Media, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open failed: {e}"))?;
    let reader = std::io::BufReader::new(file);
    let decoder = image::codecs::gif::GifDecoder::new(reader)
        .map_err(|e| format!("gif decode failed: {e}"))?;
    let mut frames = Vec::new();
    let mut delays = Vec::new();
    for (i, frame) in decoder.into_frames().enumerate() {
        if i >= MAX_GIF_FRAMES {
            break;
        }
        let frame = frame.map_err(|e| format!("gif frame failed: {e}"))?;
        let delay = Duration::from(frame.delay());
        let buf = fit_down(frame.into_buffer(), MAX_GIF_DIM);
        frames.push(buf);
        delays.push(if delay.is_zero() {
            Duration::from_millis(100)
        } else {
            delay
        });
    }
    if frames.is_empty() {
        return Err(format!("{name}: GIFにフレームがありません"));
    }
    Ok(Media::Animated {
        name: format!("{name} ({} frames)", frames.len()),
        frames,
        delays,
        started: Instant::now(),
        speed: 1.0,
        repeat: true,
    })
}

/// 大きすぎる画像は長い辺を上限に縮小 (表示・blur負荷対策)。
fn fit_down(img: RgbaImage, max_side: u32) -> RgbaImage {
    let (w, h) = (img.width(), img.height());
    if w.max(h) <= max_side {
        return img;
    }
    let scale = max_side as f32 / w.max(h) as f32;
    image::imageops::resize(
        &img,
        (w as f32 * scale) as u32,
        (h as f32 * scale) as u32,
        image::imageops::Triangle,
    )
}

#[allow(dead_code)]
pub fn load_dialog() -> Option<std::path::PathBuf> {
    rfd::FileDialog::new()
        .add_filter(
            "Images / GIF / Video",
            &[
                "png", "jpg", "jpeg", "gif", "bmp", "webp", "mp4", "mov", "m4v", "avi", "mkv",
                "webm",
            ],
        )
        .pick_file()
}

// ---------- Video (ffmpeg-sidecar) ----------

/// 別プロセスdecode→rawvideo pipe。Dropで子プロセスkill。
pub struct VideoStream {
    name: String,
    pub w: u32,
    pub h: u32,
    pub fps_label: String,
    pub paused: bool,
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    child: Option<ffmpeg_sidecar::child::FfmpegChild>,
    last: RgbaImage,
    frame_no: u64,
    dead: bool,
}

impl std::fmt::Debug for VideoStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoStream")
            .field("name", &self.name)
            .field("w", &self.w)
            .field("h", &self.h)
            .field("frame_no", &self.frame_no)
            .finish()
    }
}

impl Drop for VideoStream {
    fn drop(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
        }
    }
}

impl VideoStream {
    /// 到着分をdrain→最新化。進展時のみtrue。
    pub fn poll(&mut self) -> bool {
        if self.paused || self.dead {
            return false;
        }
        let mut advanced = false;
        while let Ok(raw) = self.rx.try_recv() {
            if raw.len() == (self.w * self.h * 4) as usize {
                if let Some(img) = image::ImageBuffer::from_raw(self.w, self.h, raw) {
                    self.last = img;
                    self.frame_no += 1;
                    advanced = true;
                }
            } else if raw.is_empty() {
                // 空パケット=EOF通知
                self.dead = true;
                break;
            }
        }
        advanced
    }
}

/// ffmpeg/ffprobeの解決: PATH優先、無ければbrew定番パス。
fn resolve_tool(name: &str) -> Option<PathBuf> {
    if std::process::Command::new(name)
        .arg("-version")
        .output()
        .is_ok()
    {
        return Some(PathBuf::from(name));
    }
    for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        let p = PathBuf::from(dir).join(name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// 速度・リピート指定付き動画読込。変更時はspawnし直し。
pub fn load_video_speed(
    path: &Path,
    name: &str,
    speed: f32,
    repeat: bool,
) -> Result<Media, String> {
    let ffmpeg = resolve_tool("ffmpeg")
        .ok_or_else(|| "ffmpegが見つかりません (brew install ffmpeg)".to_string())?;
    let ffprobe = resolve_tool("ffprobe")
        .ok_or_else(|| "ffprobeが見つかりません (brew install ffmpeg)".to_string())?;

    // 寸法・fps取得
    let out = std::process::Command::new(&ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,avg_frame_rate",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe実行失敗: {e}"))?;
    if !out.status.success() {
        return Err(format!("{name}: 映像ストリームがありません"));
    }
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("ffprobe解析失敗: {e}"))?;
    let stream = v
        .pointer("/streams/0")
        .ok_or_else(|| format!("{name}: 映像ストリームがありません"))?;
    let sw = stream["width"].as_u64().unwrap_or(0) as u32;
    let sh = stream["height"].as_u64().unwrap_or(0) as u32;
    if sw == 0 || sh == 0 {
        return Err(format!("{name}: サイズ取得失敗"));
    }
    let fps_label = stream["avg_frame_rate"].as_str().unwrap_or("?").to_string();
    // 出力サイズ (アスペクト維持・偶数化・上限1280)
    let scale = (MAX_VIDEO_SIDE as f32 / sw.max(sh) as f32).min(1.0);
    let mut w = ((sw as f32 * scale) as u32).max(2);
    let mut h = ((sh as f32 * scale) as u32).max(2);
    w -= w % 2;
    h -= h % 2;

    let mut cmd = ffmpeg_sidecar::command::FfmpegCommand::new_with_path(&ffmpeg);
    // setptsで速度変更 (1.0=等速)。-reと併用で自然なペース。repeat=falseでワンショット。
    let vf = format!("setpts=PTS/{:.3},scale={w}:{h}", speed.max(0.05));
    cmd.args(["-hide_banner", "-v", "error"]);
    if repeat {
        cmd.args(["-stream_loop", "-1"]);
    }
    cmd.args(["-re", "-i"])
        .arg(path.as_os_str())
        .args(["-map", "0:v:0", "-an", "-vf", &vf, "-pix_fmt", "rgba"])
        .rawvideo();
    let mut child = cmd.spawn().map_err(|e| format!("ffmpeg起動失敗: {e}"))?;
    let mut stdout = child
        .take_stdout()
        .ok_or_else(|| "ffmpeg stdout取得失敗".to_string())?;
    // stderrは別スレッドで捨てる (詰まり防止)
    if let Some(mut stderr) = child.take_stderr() {
        std::thread::spawn(move || {
            let mut sink = std::io::sink();
            let _ = std::io::copy(&mut stderr, &mut sink);
        });
    }
    let frame_len = (w * h * 4) as usize;
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(2);
    std::thread::spawn(move || {
        let mut buf = vec![0u8; frame_len];
        loop {
            match stdout.read_exact(&mut buf) {
                Ok(()) => {
                    if tx
                        .send(std::mem::replace(&mut buf, vec![0u8; frame_len]))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(_) => {
                    let _ = tx.send(Vec::new()); // EOF通知
                    break;
                }
            }
        }
    });

    let blank = RgbaImage::from_pixel(w, h, image::Rgba([0, 0, 0, 255]));
    Ok(Media::Video {
        name: format!("{name} ({w}×{h} {fps_label}fps loop)"),
        stream: VideoStream {
            name: name.to_string(),
            w,
            h,
            fps_label,
            paused: false,
            rx,
            child: Some(child),
            last: blank,
            frame_no: 0,
            dead: false,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 実動画デコードのsmoke test。要system ffmpeg。CIではskip:
    /// `cargo test -p freemapper-app -- --ignored`
    #[test]
    #[ignore]
    fn video_decode_smoke() {
        let path = std::env::temp_dir().join("freemapper_smoke.mp4");
        let st = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=10:duration=2",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&path)
            .output()
            .expect("ffmpeg fixture");
        assert!(st.status.success(), "fixture生成失敗");

        let mut m = load_file(&path).expect("load video");
        assert!(matches!(m, Media::Video { .. }));
        // -re付きrealtime decodeなので最大10秒待つ
        let mut got = false;
        for _ in 0..100 {
            if m.poll_video() {
                got = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(got, "フレームが届かない");
        assert!(m.frame_token() > 0);
        let base = m.current_base();
        assert_eq!((base.width(), base.height()), (320, 240));
    }
}
