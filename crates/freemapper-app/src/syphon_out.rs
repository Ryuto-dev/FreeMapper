//! Syphon出力 (macOSのみ、v0.1はCPUアップロード経路)。
//! Server名 "FreeMapper" で配信。OBS/MadMapper/Resolume等から受信可。
//! 非macOSでは無効スタブ (WindowsはSpout予定)。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyphonStatus {
    Off,
    Waiting,
    Live,
    Error,
}

#[cfg(target_os = "macos")]
mod imp {
    use super::SyphonStatus;
    use syphon_rs::Server;

    pub struct SyphonOut {
        server: Option<Server>,
        pub enabled: bool,
        pub status: SyphonStatus,
        pub detail: String,
        w: u32,
        h: u32,
    }

    impl Default for SyphonOut {
        fn default() -> Self {
            Self {
                server: None,
                enabled: false,
                status: SyphonStatus::Off,
                detail: String::new(),
                w: 0,
                h: 0,
            }
        }
    }

    impl SyphonOut {
        pub fn set_enabled(&mut self, on: bool) {
            self.enabled = on;
            if on && self.server.is_none() {
                match Server::new("FreeMapper", 480, 270) {
                    Ok(s) => {
                        self.server = Some(s);
                        self.w = 480;
                        self.h = 270;
                        self.status = SyphonStatus::Waiting;
                        self.detail.clear();
                    }
                    Err(e) => {
                        self.status = SyphonStatus::Error;
                        self.detail = format!("{e:?} (~/Library/Frameworks/Syphon.framework必要)");
                    }
                }
            } else if !on {
                if let Some(s) = self.server.take() {
                    s.stop();
                }
                self.status = SyphonStatus::Off;
                self.detail.clear();
            }
        }

        /// RGBA8タイトパック画像を配信。サイズ変化時はresize。
        pub fn push(&mut self, rgba: &[u8], w: u32, h: u32) {
            let Some(s) = self.server.as_mut() else {
                return;
            };
            if self.w != w || self.h != h {
                match s.resize(w, h) {
                    Ok(()) => {
                        self.w = w;
                        self.h = h;
                    }
                    Err(e) => {
                        self.status = SyphonStatus::Error;
                        self.detail = format!("resize: {e:?}");
                        return;
                    }
                }
            }
            match s.send_frame(rgba) {
                Ok(()) => {
                    self.status = if s.has_clients() {
                        SyphonStatus::Live
                    } else {
                        SyphonStatus::Waiting
                    };
                    self.detail.clear();
                }
                Err(e) => {
                    self.status = SyphonStatus::Error;
                    self.detail = format!("send: {e:?}");
                }
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::SyphonStatus;

    #[derive(Default)]
    pub struct SyphonOut {
        pub enabled: bool,
        pub status: SyphonStatus,
        pub detail: String,
    }

    impl SyphonOut {
        pub fn set_enabled(&mut self, _on: bool) {
            self.enabled = false;
            self.status = SyphonStatus::Off;
            self.detail = "macOS only (WindowsはSpout予定)".to_string();
        }

        pub fn push(&mut self, _rgba: &[u8], _w: u32, _h: u32) {}
    }
}

pub use imp::SyphonOut;

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn server_create_send_stop() {
        let mut out = SyphonOut::default();
        out.set_enabled(true);
        assert!(
            out.status == SyphonStatus::Waiting || out.status == SyphonStatus::Live,
            "unexpected: {:?} {}",
            out.status,
            out.detail
        );
        let px = vec![128u8; 64 * 64 * 4];
        out.push(&px, 64, 64);
        assert!(
            out.status == SyphonStatus::Waiting || out.status == SyphonStatus::Live,
            "send failed: {:?} {}",
            out.status,
            out.detail
        );
        out.set_enabled(false);
        assert_eq!(out.status, SyphonStatus::Off);
    }
}
