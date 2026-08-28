# 0player

A zero-player game: a local LLM plays Game Boy games and streams its reasoning while you watch.

Runs entirely on your machine. The emulator is [gameboy](https://github.com/x0cero/gameboy), written from scratch in Rust; the model runs in [Ollama](https://ollama.com). No API keys, no cloud, no cost.

## How it works

Every turn, 0player screenshots the emulated screen, hands it to a vision model, and asks it to think out loud and end with an `ACTION:` line naming the buttons to press. The buttons are pressed, the game runs forward, and the loop repeats. A local web viewer shows the game next to the model's live token stream.

## Run it

```sh
ollama pull qwen2.5vl:7b
cargo run --release -- --rom your-game.gb
# open http://localhost:3117
```

Options:

```
--model <name>    Ollama model (default qwen2.5vl:7b)
--host <url>      Ollama host (default http://localhost:11434)
--goal <text>     what the model should try to do
--port <n>        viewer port (default 3117)
```

Bring your own ROM. Homebrew games like [Libbet and the Magic Floor](https://github.com/pinobatch/libbet) are free and legal to download.

## Status

Early. The model sees raw pixels only; it plays slowly and gets confused, which is half the fun. Planned: structured game-state adapters (starting with Pokémon Red RAM maps) so it can actually make progress, and Game Boy Color + GBA support.

## License

MIT
