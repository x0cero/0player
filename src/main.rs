mod adapter;
mod agent;
mod emu;
mod llm;
mod runtime;
mod server;

use std::sync::Arc;

fn usage() -> ! {
    eprintln!(
        "0player: a local LLM plays Game Boy games\n\n\
         usage: 0player --rom <path.gb> [options]\n\n\
         options:\n\
           --model <name>    Ollama model (default qwen2.5vl:7b)\n\
           --host <url>      Ollama host (default http://localhost:11434)\n\
           --goal <text>     what the model should try to do\n\
           --port <n>        viewer port (default 3117)\n"
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("--path") {
        path_main(&args[2], args[3].parse().unwrap(), args[4].parse().unwrap());
        return;
    }
    if args.get(1).map(|s| s.as_str()) == Some("--probe") {
        probe_main(&args[2], args[3].parse().unwrap(), args[4].parse().unwrap());
        return;
    }
    let mut rom = None;
    let mut model = "qwen2.5vl:7b".to_string();
    let mut host = "http://localhost:11434".to_string();
    let mut goal = "Explore and make progress in the game.".to_string();
    let mut port: u16 = 3117;

    let mut i = 1;
    while i < args.len() {
        let need = |i: usize| args.get(i + 1).cloned().unwrap_or_else(|| usage());
        match args[i].as_str() {
            "--rom" => rom = Some(need(i)),
            "--model" => model = need(i),
            "--host" => host = need(i),
            "--goal" => goal = need(i),
            "--port" => port = need(i).parse().unwrap_or_else(|_| usage()),
            _ => usage(),
        }
        i += 2;
    }
    let rom = rom.unwrap_or_else(|| usage());


    let emu = match emu::Emulator::new(&rom) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("failed to load {rom}: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("game: {}", emu.title());
    eprintln!("model: {model}");

    let shared = Arc::new(server::Shared::new());
    let llm = llm::Ollama::new(&host, &model);
    let is_gba = rom.to_ascii_lowercase().ends_with(".gba");
    let rt_cfg = runtime::RuntimeConfig {
        hold_frames: if is_gba { 20 } else { 8 },
        gap_frames: 10,
        state_path: if is_gba { format!("{rom}.0pstate") } else { String::new() },
        notes_path: format!("{rom}.0pnotes"),
        publish_every: 6, // ~10 viewer frames per second
    };
    let handle = runtime::spawn(emu, rt_cfg, shared.clone());

    let cfg = agent::AgentConfig {
        goal,
        scale: 3,
        history_turns: 6,
        notes_path: format!("{rom}.0pnotes"),
        guide_path: format!("{rom}.0pguide"),
    };
    let agent_shared = shared.clone();
    std::thread::spawn(move || agent::run(handle, llm, cfg, agent_shared));
    server::serve(shared, port);
}

/// Hidden calibration: print metatile info around a tile and exit.
/// Usage: 0player --probe <rom> <x> <y>
pub fn probe_main(rom: &str, x: i32, y: i32) {
    let mut emu = emu::Emulator::new(rom).expect("rom");
    emu.run_frames(120);
    if let Ok(state) = std::fs::read(format!("{rom}.0pstate")) {
        emu.state_load(&state);
    }
    emu.run_frames(2);
    adapter::debug_metatile(&mut emu, x, y);
}

/// Hidden: test pathfinding from the saved state. Usage: 0player --path <rom> <x> <y>
pub fn path_main(rom: &str, x: i32, y: i32) {
    let mut emu = emu::Emulator::new(rom).expect("rom");
    emu.run_frames(120);
    if let Ok(state) = std::fs::read(format!("{rom}.0pstate")) {
        println!("state loaded: {}", emu.state_load(&state));
    }
    emu.run_frames(2);
    println!("player at {:?}", adapter::coords(&mut emu));
    match adapter::find_path(&mut emu, x, y, &std::collections::HashSet::new()) {
        Ok(steps) => println!("path ({} steps): {:?}", steps.len(), steps.iter().map(|b| b.name()).collect::<Vec<_>>()),
        Err(e) => println!("ERR: {e}"),
    }
}
