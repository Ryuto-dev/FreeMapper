//! OSC受信 (v0.1: 固定マップ・Learnはv0.2)。
//! 別スレッドでUDP受信→チャネル→UIスレッドが毎フレーム適用。
//!
//! アドレス (prefix `/freemapper`):
//! - `/fx/brightness f` (-100..100), `/fx/contrast f`, `/fx/saturation f` (0..200),
//!   `/fx/hue f` (-180..180), `/fx/blur f` (0..10)
//! - `/gen/select i` (0..5: TestCard..Bars), `/surface/active i`, `/output/fullscreen i` (0/1)

use std::net::UdpSocket;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{channel, Receiver, Sender},
    Arc,
};
use std::time::Duration;

use rosc::{OscMessage, OscPacket, OscType};

pub const DEFAULT_OSC_PORT: u16 = 8000;

/// 受信イベント: (OSCアドレス, コマンド)
type OscEvent = (String, OscCmd);

#[derive(Debug, Clone)]
pub enum OscCmd {
    Brightness(f32),
    Contrast(f32),
    Saturation(f32),
    Hue(f32),
    Blur(f32),
    Generator(usize),
    Surface(usize),
    Fullscreen(bool),
}

/// 受信スレッドのハンドル。Dropで停止。
pub struct OscServer {
    pub port: u16,
    pub running: bool,
    pub received: u64,
    pub last_addr: String,
    pub detail: String,
    rx: Option<Receiver<OscEvent>>,
    stop: Option<Arc<AtomicBool>>,
}

impl Default for OscServer {
    fn default() -> Self {
        Self {
            port: DEFAULT_OSC_PORT,
            running: false,
            received: 0,
            last_addr: String::new(),
            detail: String::new(),
            rx: None,
            stop: None,
        }
    }
}

impl OscServer {
    pub fn start(&mut self, port: u16) {
        self.stop();
        self.port = port;
        let sock = match UdpSocket::bind(format!("0.0.0.0:{port}")) {
            Ok(s) => s,
            Err(e) => {
                self.running = false;
                self.detail = format!("bind失敗: {e}");
                return;
            }
        };
        if sock
            .set_read_timeout(Some(Duration::from_millis(200)))
            .is_err()
        {
            self.running = false;
            self.detail = "timeout設定失敗".to_string();
            return;
        }
        let (tx, rx): (Sender<OscEvent>, Receiver<OscEvent>) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_c = stop.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while !stop_c.load(Ordering::Relaxed) {
                match sock.recv_from(&mut buf) {
                    Ok((n, _src)) => {
                        if let Ok((_, OscPacket::Message(msg))) =
                            rosc::decoder::decode_udp(&buf[..n])
                        {
                            if let Some(cmd) = map_message(&msg) {
                                if tx.send((msg.addr.clone(), cmd)).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                    Err(_) => break,
                }
            }
        });
        self.rx = Some(rx);
        self.stop = Some(stop);
        self.running = true;
        self.detail.clear();
    }

    pub fn stop(&mut self) {
        if let Some(s) = self.stop.take() {
            s.store(true, Ordering::Relaxed);
        }
        self.rx = None;
        self.running = false;
    }

    /// UIスレッド側: 溜まったコマンドを全部取り出す。
    pub fn drain(&mut self) -> Vec<OscCmd> {
        let mut out = Vec::new();
        if let Some(rx) = &self.rx {
            while let Ok((addr, cmd)) = rx.try_recv() {
                self.received += 1;
                self.last_addr = addr;
                out.push(cmd);
            }
        }
        out
    }
}

fn first_float(msg: &OscMessage) -> Option<f32> {
    msg.args.first().and_then(|a| match a {
        OscType::Float(f) => Some(*f),
        OscType::Double(d) => Some(*d as f32),
        OscType::Int(i) => Some(*i as f32),
        _ => None,
    })
}

fn first_int(msg: &OscMessage) -> Option<i32> {
    msg.args.first().and_then(|a| match a {
        OscType::Int(i) => Some(*i),
        OscType::Float(f) => Some(*f as i32),
        _ => None,
    })
}

fn map_message(msg: &OscMessage) -> Option<OscCmd> {
    let addr = msg.addr.strip_prefix("/freemapper")?;
    Some(match addr {
        "/fx/brightness" => OscCmd::Brightness(first_float(msg)?),
        "/fx/contrast" => OscCmd::Contrast(first_float(msg)?),
        "/fx/saturation" => OscCmd::Saturation(first_float(msg)?),
        "/fx/hue" => OscCmd::Hue(first_float(msg)?),
        "/fx/blur" => OscCmd::Blur(first_float(msg)?),
        "/gen/select" => OscCmd::Generator(first_int(msg)?.max(0) as usize),
        "/surface/active" => OscCmd::Surface(first_int(msg)?.max(0) as usize),
        "/output/fullscreen" => OscCmd::Fullscreen(first_int(msg).unwrap_or(0) != 0),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rosc::encoder;

    fn send(addr: &str, arg: OscType, port: u16) {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let packet = OscPacket::Message(OscMessage {
            addr: addr.to_string(),
            args: vec![arg],
        });
        let buf = encoder::encode(&packet).unwrap();
        sock.send_to(&buf, format!("127.0.0.1:{port}")).unwrap();
    }

    #[test]
    fn osc_self_talk() {
        let port = 18099;
        let mut srv = OscServer::default();
        srv.start(port);
        assert!(srv.running, "bind失敗: {}", srv.detail);
        // 少し待ってから送信 (bind確立)
        std::thread::sleep(Duration::from_millis(100));
        send("/freemapper/fx/brightness", OscType::Float(42.0), port);
        send("/freemapper/gen/select", OscType::Int(2), port);
        send("/unknown/addr", OscType::Int(1), port);
        // 到着待ち
        let mut cmds = Vec::new();
        for _ in 0..50 {
            cmds.extend(srv.drain());
            if cmds.len() >= 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(cmds.len(), 2, "不明アドレスは無視される");
        assert!(matches!(cmds[0], OscCmd::Brightness(f) if (f - 42.0).abs() < 1e-6));
        assert!(matches!(cmds[1], OscCmd::Generator(2)));
        srv.stop();
        assert!(!srv.running);
    }
}
