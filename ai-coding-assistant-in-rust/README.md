# rai — a local AI coding assistant, entirely in Rust

`rai` is a command-line coding assistant built around a specific thesis: the hard
part of an AI coding assistant is not the prompt, it is the runtime. Streaming,
models, and tool-calling are largely solved. Editing safely is not.

So this project treats the assistant as a **governed system**: the model
proposes, and this Rust runtime owns policy, state, tool execution, and limits.

> The model predicts tokens. The runtime enforces reality.

```console
$ rai ask "where is the policy engine defined?" --provider local
[session]   ask (answer questions using repository search and file reads) ...
[context]   indexing C:\work\my-project
[notice]    indexed 42 file(s), 318 symbol(s), 0 skipped as binary
[tool]      search_text (ReadOnly) (query=policy, max_results=30)
[tool]      search_text ok in 4ms - 7 match(es) for `policy`
...
```

## What it is

- A single binary that can read a repository, answer questions about it,
  propose and apply patches, run approved commands, review a git diff, and
  expose itself over MCP.
- Offline-capable: a built-in heuristic provider answers `ask` without any
  network access or API key, and `review` never needs a model at all.
- Honest about failure: every run leaves a factual work log, and nothing claims
  to have happened that did not.

## What it is not

- Not an autonomous agent that can do anything on your machine. `ask` cannot
  write files. `edit` cannot run arbitrary commands unless you opt in. Nothing
  touches the disk except through a patch.
- Not a chat UI. Output is a terminal event stream (text or JSONL).
- Not a promise that a 7B model will refactor your codebase. Small models are
  good at context retrieval and constrained edits, and the design targets that.

## Trust model

Every tool declares a **risk class**, and policy is written against classes:

| Risk class | Examples | Default behaviour |
| --- | --- | --- |
| `ReadOnly` | `list_files`, `read_file`, `search_text`, `list_symbols`, `git_status`, `git_diff` | Allowed |
| `WorkspaceWrite` | `apply_patch`, `write_file` | Needs approval, or a config `auto_approve` entry |
| `Command` | `run_command`, `run_tests` | Allowlisted argv runs; everything else needs approval |
| `Network` | `fetch_docs`, MCP server tools | Needs approval |
| `Destructive` | deleting files, `git reset --hard` | Refused unless `allow_destructive = true` **and** approved |
| `CredentialSensitive` | secrets, cloud accounts | Refused by default |

Four properties are enforced by construction rather than by prompt:

1. **The mode decides what is even advertised.** `ask` never receives a write
   or command tool. `edit` receives writes, and commands only with
   `--allow-commands` or `--verify`. A model cannot call a tool it was not
   given, and if it tries anyway the runtime refuses the call.
2. **One write path.** Files change only through `apply_patch`, with a unified
   diff. Patches are reviewable, auditable, and rejectable. There is no
   "write any file" primitive.
3. **The sandbox owns execution.** Explicit argv, workspace-confined working
   directory, filtered environment, timeout with kill, output caps, separate
   stdout/stderr, exit codes, and redaction of credential-shaped strings.
   Secret-named environment variables never reach a child process, even when the
   allowlist names them.
4. **Budgets are enforced, not suggested.** Tool calls, model turns, wall clock,
   patch size, and command output are all capped in code, so "agentic" cannot
   silently become "unbounded".

## Quick start

```bash
cargo build --release

cd /path/to/your-project
rai init                       # writes .rai/config.toml and .rai/memories.md
rai index                      # builds the local repository index
rai config --check             # validates the config and prints the tool surface

# Offline: retrieval plus extractive answers, no network, no key.
rai ask "how does the parser handle errors?" --provider local

# With a real model (OpenAI, or any OpenAI-compatible server).
export OPENAI_API_KEY=...
rai ask "how does the parser handle errors?"
```

Then:

```bash
rai edit "replace in src/lib.rs: \"let x = 1;\" -> \"let x = 2;\"" --provider local --dry-run
rai edit "add a Display impl for Config" --verify        # verifies with cargo check
rai review                                               # deterministic risk scan of the diff
rai run cargo test                                       # through the sandbox and policy
```

Full walkthrough, configuration reference, and troubleshooting: **[USAGE.md](USAGE.md)**.

## Providers

| Provider | Needs | What it does |
| --- | --- | --- |
| `local` | nothing | Offline heuristic planner: ranks repository matches with real tools, and answers extractively. It does not pretend to reason. |
| `scripted` | a JSON script | Replays fixed responses, including tool calls. Used by the test suite and for reproducible demos. |
| `openai` | `base_url` (+ key if hosted) | Any `/chat/completions` server: OpenAI, Ollama, LM Studio, vLLM, DeepSeek. Streaming and tool calls included. |

The provider is the only swappable external dependency. Policy, tools, patches,
budgets, and logging are provider-agnostic by design.

## MCP, both directions

- **Client:** `rai mcp list-tools` and `rai mcp call <server.tool> --json-args '{}'`
  connect to servers declared in `[[mcp.servers]]`. Calls that are not
  allowlisted go through the same policy engine as everything else.
- **Server:** `rai mcp-serve` exposes this project's tools over stdio:
  `project.search`, `project.read_file`, `project.apply_patch`,
  `project.run_tests`, `project.summarize_diff`, `project.explain_failure`.

The server is the interesting direction, because the tool surface is a design
decision. `project.apply_patch` is better than `write_any_file`;
`project.run_tests` is safer than `shell`; `project.read_file` is safer than
`read_absolute_path`. MCP standardizes communication; it does not replace
authorization design.

## Architecture

| Module | Responsibility |
| --- | --- |
| `cli.rs`, `commands.rs` | The CLI is the first policy layer: modes, flags, exit codes. |
| `config.rs` | Explicit, validated, reviewable configuration; typo-proof with actionable errors. |
| `policy.rs` | Risk classes, allowlists, approval decisions. The trust boundary. |
| `tools/` | Tool registry with schemas, risk classes, and execution modes. |
| `patch.rs` | Unified-diff parsing and application, content-anchored and workspace-confined. |
| `sandbox.rs` | Command execution with timeouts, caps, env filtering, redaction, cancellation. |
| `index.rs` | Repository context: paths, languages, symbols, structured search. |
| `git.rs` | Status, diffs, and dirty-file detection. |
| `model/` | One internal interface; adapters for OpenAI-compatible, local, and scripted providers. |
| `agent.rs` | The loop: model turns, policy checks, tool execution, budgets, verification. |
| `events.rs`, `render.rs` | The event model and its terminal/JSONL renderers. |
| `session.rs`, `memory.rs` | Durable transcript, compaction, resumption, and project memory. |
| `mcp/` | MCP client and server, both behind the same registry and policy. |
| `redact.rs` | Secret redaction for output and logs. |

## Tests

```bash
cargo test
```

219 tests: 159 unit tests plus 60 integration tests that drive the real binary,
the real tool registry, and a scripted model.

- `tests/agent_loop.rs` — end-to-end runs: tool use, budgets, refusals in read
  mode, patch application, dry runs, cancellation, session recording.
- `tests/patch_and_policy.rs` — patch application, path confinement, binary and
  size guards, dirty-file reporting, and the command policy gate.
- `tests/command_sandbox.rs` — stream separation, exit codes, environment
  filtering, redaction, truncation, and hard timeouts.
- `tests/cli_surface.rs` — `init`, `config`, `index`, `memory`, `sessions`,
  `review` (including its CI-friendly exit code), and JSONL output.
- `tests/mcp_roundtrip.rs` — this crate's MCP client against the real
  `rai mcp-serve` binary, including a policy refusal.
- `tests/openai_adapter.rs` — the OpenAI-compatible adapter against a local
  server that speaks the wire protocol: request shape, SSE streaming, tool-call
  fragment accumulation, and usage parsing.

### Verification scope

Verified in the development environment:

- `cargo build`, `cargo clippy --all-targets`, `cargo fmt --check`, `cargo test`.
- Manual CLI runs of `init`, `index`, `config`, `ask`, `edit`, `review`, `run`,
  `memory`, `sessions`, and `mcp-serve` against real workspaces.
- The MCP server driven by this crate's MCP client over stdio.
- The OpenAI-compatible adapter against a local socket server implementing the
  protocol.

Not verified here: a call to a live hosted model API, because no API key and no
running local model server were present. The adapter path is covered by
`tests/openai_adapter.rs`; pointing `--base-url` at Ollama, LM Studio, or a
hosted endpoint is the remaining step, and it is a configuration change rather
than a code change.

## License

MIT OR Apache-2.0.
