//! Local viewer: serves the page, the latest frame, and an SSE stream of the
//! model's reasoning.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Mutex;
use std::sync::Arc;

pub enum Event {
    TurnStart { turn: u64 },
    Token(String),
    Action(String),
    Error(String),
}

impl Event {
    fn sse(&self) -> String {
        let (kind, data) = match self {
            Event::TurnStart { turn } => ("turn", turn.to_string()),
            Event::Token(t) => ("token", serde_json::to_string(t).unwrap()),
            Event::Action(a) => ("action", serde_json::to_string(a).unwrap()),
            Event::Error(e) => ("error", serde_json::to_string(e).unwrap()),
        };
        format!("event: {kind}\ndata: {data}\n\n")
    }
}

pub struct Shared {
    frame: Mutex<Vec<u8>>, // latest PNG
    listeners: Mutex<Vec<Sender<String>>>,
    paused: AtomicBool,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            frame: Mutex::new(Vec::new()),
            listeners: Mutex::new(Vec::new()),
            paused: AtomicBool::new(false),
        }
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn publish_frame(&self, png: &[u8]) {
        *self.frame.lock().unwrap() = png.to_vec();
        self.publish_raw("event: frame\ndata: 1\n\n".to_string());
    }

    pub fn publish(&self, ev: Event) {
        self.publish_raw(ev.sse());
    }

    fn publish_raw(&self, msg: String) {
        let mut ls = self.listeners.lock().unwrap();
        ls.retain(|tx| tx.send(msg.clone()).is_ok());
    }
}

pub fn serve(shared: Arc<Shared>, port: u16) {
    let server = tiny_http::Server::http(("127.0.0.1", port)).expect("bind viewer port");
    eprintln!("viewer: http://localhost:{port}");
    for request in server.incoming_requests() {
        let shared = shared.clone();
        // Match on the path only; the viewer adds ?t= cache-busters.
        let path = request.url().split('?').next().unwrap_or("/").to_string();
        match path.as_str() {
            "/" => {
                let resp = tiny_http::Response::from_string(INDEX_HTML).with_header(
                    tiny_http::Header::from_bytes("Content-Type", "text/html; charset=utf-8")
                        .unwrap(),
                );
                let _ = request.respond(resp);
            }
            "/frame.png" => {
                let png = shared.frame.lock().unwrap().clone();
                let resp = tiny_http::Response::from_data(png).with_header(
                    tiny_http::Header::from_bytes("Content-Type", "image/png").unwrap(),
                );
                let _ = request.respond(resp);
            }
            "/pause" => {
                let now = !shared.paused.load(Ordering::Relaxed);
                shared.paused.store(now, Ordering::Relaxed);
                let _ = request.respond(tiny_http::Response::from_string(if now {
                    "paused"
                } else {
                    "running"
                }));
            }
            "/events" => {
                let (tx, rx) = channel::<String>();
                shared.listeners.lock().unwrap().push(tx);
                std::thread::spawn(move || {
                    let mut writer = request.into_writer();
                    let _ = writer.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n",
                    );
                    let _ = writer.flush();
                    while let Ok(msg) = rx.recv() {
                        if writer.write_all(msg.as_bytes()).is_err() || writer.flush().is_err() {
                            break;
                        }
                    }
                });
            }
            _ => {
                let _ = request.respond(tiny_http::Response::from_string("not found").with_status_code(404));
            }
        }
    }
}

const INDEX_HTML: &str = include_str!("viewer.html");
