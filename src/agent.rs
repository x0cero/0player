//! The agent loop: look at the screen, think out loud, press buttons.

use crate::emu::{Button, Emulator};
use crate::llm::{Message, Ollama};
use crate::server::{Event, Shared};
use base64::Engine;
use std::sync::Arc;

const SYSTEM_PROMPT: &str = "You are playing a Game Boy game. Each turn you see a screenshot of the current screen.\n\
Think briefly about what is happening and what to do next, then end your reply with exactly one line:\n\
ACTION: <buttons>\n\
where <buttons> is AT MOST 5 button names separated by spaces, chosen from: A B START SELECT UP DOWN LEFT RIGHT.\n\
Never write more than 5 buttons; anything past the fifth is ignored.\n\
Buttons are pressed one after another, one tap each. Examples:\n\
ACTION: A\n\
ACTION: UP UP A\n\
Rules of thumb: START opens menus or begins the game from a title screen; A confirms and talks to people or advances text; B cancels; the d-pad moves the character or the menu cursor.\n\
Keep your thinking to a few sentences. If the screen did not change since last turn, your last action did nothing, so try something different.";

pub struct AgentConfig {
    pub goal: String,
    pub scale: u32,
    pub hold_frames: u32,
    pub settle_frames: u32,
    pub history_turns: usize,
}

pub fn run(mut emu: Emulator, llm: Ollama, cfg: AgentConfig, shared: Arc<Shared>) {
    // Boot past the logo before the first look.
    emu.run_frames(120);

    let mut history: Vec<(String, String)> = Vec::new(); // (assistant reply, action taken)
    let mut turn: u64 = 0;
    let mut last_png: Vec<u8> = Vec::new();
    let mut stuck_turns: u32 = 0;
    let mut tried_while_stuck: Vec<&'static str> = Vec::new();

    loop {
        if shared.paused() {
            std::thread::sleep(std::time::Duration::from_millis(200));
            continue;
        }
        turn += 1;
        let png = emu.screenshot_png(cfg.scale);
        let unchanged = png == last_png;
        if unchanged {
            stuck_turns += 1;
        } else {
            stuck_turns = 0;
            tried_while_stuck.clear();
        }
        last_png = png.clone();
        shared.publish_frame(&emu.screenshot_png(2));

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

        if buttons.is_empty() {
            // No parsable action: let time pass so the screen can change.
            emu.run_frames(30);
        }
        for b in &buttons {
            emu.press(*b, cfg.hold_frames, cfg.settle_frames);
            let name = b.name();
            if !tried_while_stuck.contains(&name) {
                tried_while_stuck.push(name);
            }
        }

        history.push((reply, action_str));
        if history.len() > 32 {
            history.remove(0);
        }
    }
}

fn parse_action(reply: &str) -> Vec<Button> {
    // Take the LAST "ACTION:" line so thinking about actions doesn't count.
    let line = reply
        .lines()
        .rev()
        .find(|l| l.trim_start().to_ascii_uppercase().starts_with("ACTION:"));
    let Some(line) = line else { return Vec::new() };
    let rest = &line.trim_start()[7..];
    rest.split_whitespace()
        .filter_map(Button::parse)
        .take(5)
        .collect()
}
