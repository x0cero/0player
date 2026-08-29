//! The agent loop: look at the screen, think out loud, press buttons. The
//! emulator runs in real time on its own thread (see runtime.rs); this loop
//! only samples it and injects inputs.

use crate::emu::Button;
use crate::llm::{Message, Ollama};
use crate::runtime::EmuHandle;
use crate::server::{Event, Shared};
use base64::Engine;
use std::sync::Arc;

const SYSTEM_PROMPT: &str = "You are playing a Game Boy game. Each turn you see a screenshot of the current screen.\n\
The game runs in REAL TIME and keeps running while you think.\n\
Reply as JSON with fields: \"thought\" (2-3 sentences at most on what is happening and your plan), \"action\" (EITHER 1-5 button names separated by spaces, chosen from: A B START SELECT UP DOWN LEFT RIGHT, OR the word GOTO followed by a tile coordinate, like GOTO 12 5, which walks you automatically to walkable tile x=12,y=5 on the current map using pathfinding - prefer GOTO for all overworld travel), optionally \"note\" (ONE short lesson worth remembering across play sessions: a map fact, a trigger, a trap; only when you learn something durable), and optionally \"objective\" (your CURRENT mission in one line; set it when your mission changes and it will be shown back to you every turn until you change it; keep it stable while working on one thing).\n\
Buttons are pressed one after another, one tap each. Example: {\"thought\": \"A dialog box is open, I'll advance it.\", \"action\": \"A\", \"note\": \"The lab exit mat is at the bottom-left of the room.\"}\n\
Rules of thumb: START opens menus or begins the game from a title screen; A confirms and talks to people or advances text; B cancels; the d-pad moves the character or the menu cursor.\n\
If the screen did not change since last turn, your last action did nothing, so try something different.";

pub struct AgentConfig {
    pub goal: String,
    pub scale: u32,
    pub history_turns: usize,
    /// Persistent per-game notebook; lessons survive restarts. Empty disables.
    pub notes_path: String,
    /// Optional hand-written game guide loaded verbatim into the prompt.
    pub guide_path: String,
}

pub fn run(emu: EmuHandle, llm: Ollama, cfg: AgentConfig, shared: Arc<Shared>) {
    let mut history: Vec<(String, String)> = Vec::new(); // (assistant reply, action taken)
    let mut turn: u64 = 0;
    let mut last_png: Vec<u8> = Vec::new();
    let mut stuck_turns: u32 = 0;
    let mut tried_while_stuck: Vec<&'static str> = Vec::new();
    let mut recent_positions: Vec<String> = Vec::new();
    let mut objective = String::new();
    let notes = if cfg.notes_path.is_empty() {
        String::new()
    } else {
        std::fs::read_to_string(&cfg.notes_path).unwrap_or_default()
    };
    // A hand-written game manual, distinct from the model's own notebook.
    let guide = if cfg.guide_path.is_empty() {
        String::new()
    } else {
        std::fs::read_to_string(&cfg.guide_path).unwrap_or_default()
    };

    loop {
        if shared.paused() {
            std::thread::sleep(std::time::Duration::from_millis(200));
            continue;
        }
        turn += 1;
        let (png, game_state) = emu.screenshot(cfg.scale);
        if let Some(s) = &game_state {
            shared.publish(Event::GameState(s.clone()));
        }
        let unchanged = png == last_png;
        if unchanged {
            stuck_turns += 1;
        } else {
            stuck_turns = 0;
            tried_while_stuck.clear();
        }
        last_png = png.clone();

        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        let mut messages = vec![Message {
            role: "system".into(),
            content: {
                let mut c = format!("{SYSTEM_PROMPT}\nYour goal: {}", cfg.goal);
                if !guide.trim().is_empty() {
                    c.push_str("\nGame guide:\n");
                    c.push_str(guide.trim());
                }
                if !notes.trim().is_empty() {
                    // Keep the freshest lessons if the notebook grows long.
                    let tail: Vec<&str> = notes.lines().rev().take(50).collect();
                    let tail: Vec<&str> = tail.into_iter().rev().collect();
                    c.push_str("\nLessons you saved in previous sessions:\n");
                    c.push_str(&tail.join("\n"));
                }
                c
            },
            images: None,
        }];
        for (reply, action) in history.iter().rev().take(cfg.history_turns).rev() {
            messages.push(Message {
                role: "assistant".into(),
                content: reply.clone(),
                images: None,
            });
            messages.push(Message {
                role: "user".into(),
                content: format!("(you pressed: {action})"),
                images: None,
            });
        }
        let all_buttons = ["A", "B", "START", "SELECT", "UP", "DOWN", "LEFT", "RIGHT"];
        let content = if stuck_turns >= 2 {
            let untried: Vec<&str> = all_buttons
                .iter()
                .filter(|b| !tried_while_stuck.contains(*b))
                .copied()
                .collect();
            format!(
                "Here is the current screen. It has been IDENTICAL for {} turns. \
                 Since it last changed you already tried: {}. Those do nothing here. \
                 You MUST pick from the buttons you have not tried yet: {}.",
                stuck_turns + 1,
                tried_while_stuck.join(" "),
                if untried.is_empty() {
                    "(all tried; try pressing the same button several times in a row, like UP UP UP)".to_string()
                } else {
                    untried.join(" ")
                }
            )
        } else if unchanged {
            "Here is the current screen. It looks IDENTICAL to last turn, so your last action did nothing.".to_string()
        } else {
            "Here is the current screen.".to_string()
        };
        let content = match &game_state {
            Some(s) => {
                // Breadcrumbs: coordinates over the last several turns let
                // the model notice it is pacing in a loop.
                if let Some(xy) = s
                    .split("x=")
                    .nth(1)
                    .and_then(|r| r.split('.').next())
                    .map(|r| format!("({})", r.replace(" y=", ",").trim().to_string()))
                {
                    recent_positions.push(xy);
                    if recent_positions.len() > 8 {
                        recent_positions.remove(0);
                    }
                }
                let trail = if recent_positions.len() > 1 {
                    format!(
                        "\nYour positions over the last turns, oldest first: {}. If these repeat back and forth, you are pacing in a loop; commit to one direction for several steps instead.",
                        recent_positions.join(" ")
                    )
                } else {
                    String::new()
                };
                format!("{content}\n{s}{trail}")
            }
            None => content,
        };
        let content = if objective.is_empty() {
            content
        } else {
            format!("{content}\nYour current objective (you set this yourself): {objective}")
        };
        messages.push(Message {
            role: "user".into(),
            content,
            images: Some(vec![b64]),
        });

        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "thought": {"type": "string"},
                "action": {"type": "string"},
                "note": {"type": "string"},
                "objective": {"type": "string"}
            },
            "required": ["thought", "action"]
        });
        shared.publish(Event::TurnStart { turn });
        let mut on_token = |tok: &str| shared.publish(Event::Token(tok.to_string()));
        let reply = match llm.chat(&messages, Some(&schema), &mut on_token) {
            Ok(r) => r,
            Err(e) => {
                shared.publish(Event::Error(e));
                std::thread::sleep(std::time::Duration::from_secs(3));
                continue;
            }
        };

        let (thought, mut buttons, note, new_objective) = parse_reply(&reply);
        if let Some(o) = new_objective {
            let o = o.trim().to_string();
            if !o.is_empty() {
                objective = o;
            }
        }
        // Walking blind overshoots bends and doorways; cap movement bursts
        // at 3 so the model looks again sooner. Menu mashing (A/B) keeps 5.
        if buttons
            .iter()
            .any(|b| matches!(b, Button::Up | Button::Down | Button::Left | Button::Right))
        {
            buttons.truncate(3);
        }
        if let (Some(n), false) = (&note, cfg.notes_path.is_empty()) {
            let n = n.trim();
            if !n.is_empty() && !notes.contains(n) {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&cfg.notes_path)
                {
                    let _ = writeln!(f, "- {n}");
                }
            }
        }
        let action_str = if buttons.is_empty() {
            "(nothing)".to_string()
        } else {
            buttons
                .iter()
                .map(|b| b.name())
                .collect::<Vec<_>>()
                .join(" ")
        };
        shared.publish(Event::Action(action_str.clone()));

        let goto = parse_goto(&reply);
        if let Some((gx, gy)) = goto {
            shared.publish(Event::Action(format!("GOTO {gx} {gy}")));
            emu.goto(gx, gy);
        } else {
            for b in &buttons {
                let name = b.name();
                if !tried_while_stuck.contains(&name) {
                    tried_while_stuck.push(name);
                }
            }
            emu.press(buttons);
        }

        // History gets a capped thought plus the action actually taken, so a
        // degenerate reply can't teach the model to keep rambling.
        let thought: String = thought.chars().take(240).collect();
        history.push((
            serde_json::json!({"thought": thought.trim(), "action": action_str}).to_string(),
            action_str,
        ));
        if history.len() > 32 {
            history.remove(0);
        }
    }
}

fn parse_reply(reply: &str) -> (String, Vec<Button>, Option<String>, Option<String>) {
    // Schema-constrained replies are a JSON object; be lenient about any
    // stray text around it.
    let json = reply
        .find('{')
        .and_then(|s| reply.rfind('}').map(|e| &reply[s..=e]))
        .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok());
    let Some(v) = json else {
        // Fallback: the model slipped into "ACTION: UP A" prose. Accept it.
        for line in reply.lines() {
            if let Some(pos) = line.to_ascii_uppercase().find("ACTION:") {
                let buttons = line[pos + 7..]
                    .split_whitespace()
                    .map(|t| t.trim_matches(|c: char| !c.is_ascii_alphabetic()))
                    .filter_map(Button::parse)
                    .take(5)
                    .collect();
                let thought: String = reply
                    .lines()
                    .take_while(|l| !l.to_ascii_uppercase().contains("ACTION:"))
                    .collect::<Vec<_>>()
                    .join("\n");
                return (thought.chars().take(240).collect(), buttons, None, None);
            }
        }
        return (reply.chars().take(240).collect(), Vec::new(), None, None);
    };
    let thought = v
        .get("thought")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string();
    let buttons = v
        .get("action")
        .and_then(|a| a.as_str())
        .unwrap_or_default()
        .split_whitespace()
        .map(|t| t.trim_matches(|c: char| !c.is_ascii_alphabetic()))
        .filter_map(Button::parse)
        .take(5)
        .collect();
    let note = v
        .get("note")
        .and_then(|n| n.as_str())
        .map(|n| n.chars().take(200).collect::<String>());
    let objective = v
        .get("objective")
        .and_then(|o| o.as_str())
        .map(|o| o.chars().take(200).collect::<String>());
    (thought, buttons, note, objective)
}

fn parse_goto(reply: &str) -> Option<(i32, i32)> {
    let v: serde_json::Value = reply
        .find('{')
        .and_then(|st| reply.rfind('}').map(|e| &reply[st..=e]))
        .and_then(|j| serde_json::from_str(j).ok())?;
    let a = v.get("action")?.as_str()?;
    let up = a.to_ascii_uppercase();
    let pos = up.find("GOTO")?;
    let mut nums = up[pos + 4..]
        .split(|c: char| !c.is_ascii_digit() && c != '-')
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse::<i32>().ok());
    Some((nums.next()?, nums.next()?))
}
