# rust-ai-agent

A small AI agent written in Rust, built with the [rig](https://github.com/0xPlaygrounds/rig)
framework. It connects to an LLM through [OpenRouter](https://openrouter.ai) and
demonstrates tool use: the agent can call a custom `count_words` tool to count the
words in a piece of text.

## What it does

On startup the agent is given a system preamble and a single tool (`count_words`).
It is then prompted to count the words in the sentence:

> "Rust makes AI infrastructure safer."

The model decides to call the `count_words` tool, receives the result, and replies
with a natural-language explanation printed to stdout.

## Requirements

- Rust (edition 2024; a recent stable toolchain via [rustup](https://rustup.rs))
- An [OpenRouter](https://openrouter.ai/keys) API key or an [OpenAI](https://openai.com) API key

## Setup

1. Clone the repo and enter the directory.

2. Create a `.env` file in the project root with your OpenRouter key:

   ```sh
   OPENROUTER_API_KEY=your-openrouter-api-key-here
   OPENAI_API_KEY=your-openai-api-key-here
   ```

   The program loads this file at startup via [`dotenvy`](https://crates.io/crates/dotenvy).
   A missing `.env` is not fatal — you can instead export the variable directly in
   your shell.

3. (Optional) Pick a model. The default is set in `src/main.rs`:

   ```rust
   let model = openrouter::from_env()?.completion("openai/gpt-6-luna");
   ```

   Any model id from <https://openrouter.ai/models> works. rig also exposes typed
   constants such as `openrouter::CLAUDE_3_7_SONNET` and `openrouter::GEMINI_FLASH_2_0`.

## Run

```sh
cargo run
```

## Test

```sh
cargo test
```

Runs the unit tests for the `count_words` tool.

## Dependencies

| Crate     | Purpose                                             |
| --------- | --------------------------------------------------- |
| `rig`     | Agent/LLM framework (providers, tools, agent loop)  |
| `tokio`   | Async runtime                                       |
| `dotenvy` | Loads environment variables from a `.env` file      |
| `anyhow`  | Ergonomic error handling                            |

## Project structure

```
src/main.rs   # Agent setup, the count_words tool, and unit tests
.env          # Your OPENROUTER_API_KEY (gitignored)
```

## Notes

- `OPENROUTER_API_KEY` is read by `openrouter::from_env()`; no key is hardcoded.
- `.env` is listed in `.gitignore` so your key is never committed.
- `src/main.rs` retains commented-out lines showing how to use the direct OpenAI
  provider (`OpenAI::from_env()` with `OPENAI_API_KEY`) instead of OpenRouter.
