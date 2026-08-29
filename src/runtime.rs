//! Real-time emulator runtime. The emulator runs continuously at 60fps on
//! its own thread; the agent injects button presses and samples the screen
//! through a command channel. The game world never freezes while the model
//! thinks.

use crate::emu::{Button, Emulator};
use crate::server::Shared;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub enum Cmd {
    /// Queue button taps, executed one after another in emulated time.
    Press(Vec<Button>),
    /// Reply with a PNG of the screen (plus any adapter-provided game state)
    /// once the press queue is empty, so the model always sees the state
    /// AFTER its last action.
    Screenshot {
        scale: u32,
        resp: Sender<(Vec<u8>, Option<String>)>,
    },
}

#[derive(Clone)]
pub struct EmuHandle {
    tx: Sender<Cmd>,
}

impl EmuHandle {
    pub fn press(&self, buttons: Vec<Button>) {
        let _ = self.tx.send(Cmd::Press(buttons));
    }

    /// Blocks until the queued presses have played out; returns the frame
    /// and any game-state line the adapter could read.
    pub fn screenshot(&self, scale: u32) -> (Vec<u8>, Option<String>) {
        let (resp, rx) = channel();
        let _ = self.tx.send(Cmd::Screenshot { scale, resp });
        rx.recv().unwrap_or_default()
    }
}

pub struct RuntimeConfig {
    pub hold_frames: u32,
    pub gap_frames: u32,
    pub state_path: String,
    /// Notebook file for harness-proven facts (ledge locations etc.).
    pub notes_path: String,
    /// Publish a viewer frame every this many emulated frames.
    pub publish_every: u64,
}

pub fn spawn(mut emu: Emulator, cfg: RuntimeConfig, shared: Arc<Shared>) -> EmuHandle {
    let (tx, rx): (Sender<Cmd>, Receiver<Cmd>) = channel();
    std::thread::spawn(move || {
        // Boot past the logo, then resume any saved state.
        emu.run_frames(120);
        if !cfg.state_path.is_empty() {
            if let Ok(state) = std::fs::read(&cfg.state_path) {
                emu.state_load(&state);
            }
        }

        let frame_time = Duration::from_micros(16_667);
        let mut queue: Vec<Button> = Vec::new();
        let mut phase: u32 = 0; // frames left in current hold/gap
        let mut holding: Option<Button> = None;
        let mut pending_shot: Option<(u32, Sender<(Vec<u8>, Option<String>)>)> = None;
        let mut last_pos: Option<(u16, u16)> = None;
        let mut last_map: Option<String> = None;
        let mut last_button: Option<Button> = None;
        let mut alert: Option<String> = None;
        let mut frame: u64 = 0;
        let mut next_deadline = Instant::now();

        loop {
            // Drain commands without blocking the frame clock.
            loop {
                match rx.try_recv() {
                    Ok(Cmd::Press(b)) => queue.extend(b),
                    Ok(Cmd::Screenshot { scale, resp }) => pending_shot = Some((scale, resp)),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }

            // Advance the press state machine by one frame.
            if phase == 0 {
                if let Some(b) = holding.take() {
                    emu.release(b);
                    phase = cfg.gap_frames; // let the game react before the next tap
                } else if !queue.is_empty() {
                    // A warp (doorway, stairs) teleports the player; the rest
                    // of the queued taps were planned for the old room, so
                    // drop them and let the model look again.
                    let pos = crate::adapter::coords(&mut emu);
                    let map = crate::adapter::map_id(&mut emu);
                    if let (Some((x, y)), Some((lx, ly))) = (pos, last_pos) {
                        let jump = (x as i32 - lx as i32).abs() + (y as i32 - ly as i32).abs();
                        let same_map = map.is_some() && map == last_map;
                        if same_map && y as i32 - ly as i32 >= 2 && last_button != Some(Button::Down) {
                            // Same map but suddenly 2+ tiles south: a ledge hop.
                            queue.clear();
                            let msg = format!(
                                "ALERT: you just HOPPED A LEDGE at about ({lx},{ly}) and landed at ({x},{y}); you cannot climb back up. Walk around it via the left or right side."
                            );
                            alert = Some(msg);
                            if !cfg.notes_path.is_empty() {
                                use std::io::Write;
                                if let Ok(mut f) = std::fs::OpenOptions::new()
                                    .create(true)
                                    .append(true)
                                    .open(&cfg.notes_path)
                                {
                                    let m = map.clone().unwrap_or_default();
                                    let _ = writeln!(f, "- PROVEN ledge on map {m} near ({lx},{ly}): stepping there hops you south to ({x},{y}); route around it.");
                                }
                            }
                        } else if jump > 2 {
                            queue.clear();
                        }
                    }
                    last_pos = pos.or(last_pos);
                    if map.is_some() {
                        last_map = map;
                    }
                    if let Some(b) = (!queue.is_empty()).then(|| queue.remove(0)) {
                        emu.hold(b);
                        holding = Some(b);
                        last_button = Some(b);
                        phase = cfg.hold_frames;
                    }
                }
            }
            phase = phase.saturating_sub(1);

            emu.run_frames(1);
            frame += 1;

            // The model sees the world only after its inputs played out.
            if queue.is_empty() && holding.is_none() && phase == 0 {
                if let Some((scale, resp)) = pending_shot.take() {
                    let mut state = crate::adapter::probe(&mut emu);
                    if let Some(a) = alert.take() {
                        state = Some(match state {
                            Some(s) => format!("{a}\n{s}"),
                            None => a,
                        });
                    }
                    let _ = resp.send((emu.screenshot_png(scale), state));
                }
            }

            if frame % cfg.publish_every == 0 {
                shared.publish_frame(&emu.screenshot_png(2));
            }
            if !cfg.state_path.is_empty() && frame % 600 == 0 {
                if let Some(state) = emu.state_save() {
                    let _ = std::fs::write(&cfg.state_path, state);
                }
            }

            // Pace to real time; skip sleeping if we fell behind.
            next_deadline += frame_time;
            let now = Instant::now();
            if next_deadline > now {
                std::thread::sleep(next_deadline - now);
            } else {
                next_deadline = now;
            }
        }
    });
    EmuHandle { tx }
}
