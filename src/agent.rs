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
Think briefly about what is happening and what to do next, then end your reply with EXACTLY ONE line of the form:\n\
ACTION: <buttons>\n\
Write the word ACTION exactly once in your whole reply. Multiple ACTION lines are a protocol violation and all but the first are discarded.\n\
<buttons> is AT MOST 5 button names separated by spaces, chosen from: A B START SELECT UP DOWN LEFT RIGHT.\n\
Buttons are pressed one after another, one tap each. Examples:\n\
ACTION: A\n\
ACTION: UP UP A\n\
Rules of thumb: START opens menus or begins the game from a title screen; A confirms and talks to people or advances text; B cancels; the d-pad moves the character or the menu cursor.\n\
Keep your thinking to a few sentences. If the screen did not change since last turn, your last action did nothing, so try something different.";

pub struct AgentConfig {
    pub goal: String,
    pub scale: u32,
    pub history_turns: usize,
}

pub fn run(emu: EmuHandle, llm: Ollama, cfg: AgentConfig, shared: Arc<Shared>) {
    let mut history: Vec<(String, String)> = Vec::new(); // (assistant reply, action taken)
    let mut turn: u64 = 0;
    let mut last_png: Vec<u8> = Vec::new();
    let mut stuck_turns: u32 = 0;
    let mut tried_while_stuck: Vec<&'static str> = Vec::new();
    let mut recent_positions: Vec<String> = Vec::new();

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
            content: format!("{SYSTEM_PROMPT}\nYour goal: {}", cfg.goal),
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
        messages.push(Message {
            role: "user".into(),
            content,
            images: Some(vec![b64]),
        });

        shared.publish(Event::TurnStart { turn });
        let mut on_token = |tok: &str| shared.publish(Event::Token(tok.to_string()));
        let reply = match llm.chat(&messages, &mut on_token) {
            Ok(r) => r,
            Err(e) => {
                shared.publish(Event::Error(e));
                std::thread::sleep(std::time::Duration::from_secs(3));
                continue;
            }
        };

        let buttons = parse_action(&reply);
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

        for b in &buttons {
            let name = b.name();
            if !tried_while_stuck.contains(&name) {
                tried_while_stuck.push(name);
            }
        }
        emu.press(buttons);

        // History gets ONLY the thought before the first ACTION line plus the
        // action actually taken. Feeding a degenerate button-spam reply back
        // teaches the model to keep spamming.
        let thought: String = reply
            .lines()
            .take_while(|l| !l.trim_start().to_ascii_uppercase().starts_with("ACTION:"))
            .collect::<Vec<_>>()
            .join("\n")
            .chars()
            .take(240)
            .collect();
        history.push((
            format!("{}\nACTION: {action_str}", thought.trim()),
            action_str,
        ));
        if history.len() > 32 {
            history.remove(0);
        }
    }
}

fn parse_action(reply: &str) -> Vec<Button> {
    // Take the FIRST "ACTION:" line: when a small model degenerates into a
    // list of ACTION lines, the first is its genuine choice and the rest are
    // babble.
    // Models glue the marker to prose ("...reach it.ACTION: UP"), so accept
    // ACTION: anywhere in a line, not only at the start.
    for line in reply.lines() {
        if let Some(pos) = line.to_ascii_uppercase().find("ACTION:") {
            return line[pos + 7..]
                .split_whitespace()
                .map(|t| t.trim_matches(|c: char| !c.is_ascii_alphabetic()))
                .filter_map(Button::parse)
                .take(5)
                .collect();
        }
    }
    Vec::new()
}
