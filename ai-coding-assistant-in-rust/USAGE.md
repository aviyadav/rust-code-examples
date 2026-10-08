# rai usage guide

`rai` is a command-line coding assistant. The model proposes; the Rust runtime
decides what may actually run. This guide covers installing it, configuring the
trust model, and using every command, with output captured from real runs.

Everything below was verified on Windows with `cargo test` (218 tests) plus the
end-to-end walkthrough in [Appendix A](#appendix-a-verified-walkthrough).

---

## 1. Build and install

```bash
git clone <this project>            # or copy the directory
cd ai-coding-assistant-in-rust
cargo build --release
```

The binary is `target/release/rai` (use `target/debug/rai` during development).
Add it to `PATH`, or call it by path.

Requirements:

| Tool | Needed for | Without it |
| --- | --- | --- |
| Rust + Cargo | building `rai` | — |
| `git` | `review`, git tools, dirty-file checks | those tools report "not a repository" |
| `ripgrep` (`rg`) | fast `search_text` | `rai` falls back to its built-in scan automatically |

No API key is required to start: the default provider is offline (`local`).

---

## 2. First run

```bash
cd your-project
rai init              # write .rai/config.toml and .rai/memories.md
rai index --stats     # build the local index and show what it found
rai ask "where is the retry limit set?"
```

`rai init` prints:

```text
wrote /your-project/.rai/config.toml
wrote /your-project/.rai/memories.md
note: this directory has no .gitignore; consider ignoring `.rai/logs/` and `.rai/sessions/`

next: `rai index`, then `rai ask "what does this project do?"`
```

`rai index --stats` prints a factual summary:

```text
[context]  indexing /your-project
[notice]   indexed 3 file(s), 5 symbol(s), 0 skipped as binary
files      3
bytes      361
symbols    5
binary     0
truncated  false
languages
  rust           2
  toml           1
```

Add these to `.gitignore`:

```gitignore
.rai/logs/
.rai/sessions/
.rai/indexes/
```

Keep `.rai/config.toml` and `.rai/memories.md` committed — they are the project's
declared trust model and shared facts.

### What lands in `.rai/`

| Path | Contents | Commit it? |
| --- | --- | --- |
| `config.toml` | the trust model: provider, write permissions, command allowlist, approval mode, budgets | yes |
| `memories.md` | short facts injected into every prompt | yes |
| `indexes/index.json` | paths, languages, symbols, sizes | no |
| `sessions/<id>.jsonl` | conversation + structured event transcript per run | no |
| `logs/<id>.jsonl` | the same events as plain JSONL for tooling | no |

---

## 3. Concepts

### 3.1 Modes: the CLI owns the workflow

Risk is a property of the command you type, not of something a model decides.

| Command | Purpose | May write files? | Model's tools |
| --- | --- | --- | --- |
| `rai ask` | answer questions from repository context | no | read-only + `fetch_docs` |
| `rai edit` | propose and apply patches | yes, patches only | read-only + `apply_patch` |
| `rai edit --allow-commands` / `--verify` | above, plus verification commands | yes | + `run_command`, `run_tests` |
| `rai review` | inspect the git diff, report risks | no | read-only |
| `rai run <cmd>` | execute one approved project command | only what the command does | no model involved |

`rai ask` cannot write a file even if a model asks to, and nothing is written
without approval. Both refusals are visible in the transcript:

```text
$ rai edit 'replace in src/lib.rs: "let limit = 3;" -> "let limit = 5;"'    # no --yes
[approval] apply_patch (WorkspaceWrite) -> requested: apply_patch has risk class WorkspaceWrite and is not auto-approved
[approval] apply_patch (WorkspaceWrite) -> denied: not running interactively; re-run with --yes or add `WorkspaceWrite` to [policy].auto_approve
[tool]     apply_patch was not executed: not running interactively; re-run with --yes ...
[done]     edit complete: 2 tool call(s), 0 file(s) changed, 0 command(s), 11ms
  files         unchanged
```

A mode-level refusal reads the same way; an `ask` run that asks for
`apply_patch` is told `ask mode cannot modify the workspace or run commands`.

### 3.2 Risk classes

Every tool carries a risk class, and policy is written against classes, not
names.

| Class | Meaning | Examples |
| --- | --- | --- |
| `ReadOnly` | inspecting state | `read_file`, `search_text`, `git_diff`, `list_symbols` |
| `WorkspaceWrite` | changing workspace files | `apply_patch` |
| `Command` | running project commands | `run_command`, `run_tests` |
| `Network` | leaving the machine | `fetch_docs`, MCP tool calls |
| `Destructive` | deleting or rewriting history | not implemented in this build |
| `CredentialSensitive` | touching secrets or accounts | not implemented in this build |

`Destructive` and `CredentialSensitive` are refused unless you explicitly opt in
**and** they exist; today nothing ships in those classes, so `allow_destructive`
and `allow_credential_sensitive` are forward-looking switches.

### 3.3 Approvals

Policy decides one of three things for each call: allowed, refused, or *needs a
human*. The last case is resolved by your approval mode:

| `[policy].approval` | Behaviour for a call that is not explicitly allowed |
| --- | --- |
| `prompt` (default) | asks on the terminal; without a terminal it refuses, with instructions |
| `deny` | always refuses (good for CI and for MCP servers) |
| `auto` | allows everything not explicitly denied (good for scripted runs) |

Two shortcuts exist:

- `--yes` answers "yes" to every prompt for one run. It never overrides an
  explicit refusal such as a deny-listed command or `allow_writes = false`.
- `[policy].auto_approve = ["WorkspaceWrite", "Command"]` pre-approves a risk
  class for the project.

A refusal is not silent, and it is not retried blindly:

```text
[approval] apply_patch (WorkspaceWrite) -> requested: apply_patch has risk class WorkspaceWrite and is not auto-approved
[approval] apply_patch (WorkspaceWrite) -> denied: not running interactively; re-run with --yes or add `WorkspaceWrite` to [policy].auto_approve
```

If the same call is refused or fails twice with identical arguments, the runtime
stops the run and says so instead of burning the turn budget.

### 3.4 Patches are the only write path

There is no `write_file` tool. Changes happen through `apply_patch`, which:

- accepts a unified diff (fenced ```` ```diff ```` blocks are unwrapped);
- refuses absolute paths and any path that escapes the workspace root;
- refuses binary or non-UTF-8 targets;
- enforces `[workspace].max_file_bytes` and `[budgets].max_patch_bytes`;
- validates **every** file in the patch before writing **any** of them;
- writes atomically (temp file + rename);
- reports what it changed, including files that already had uncommitted edits.

### 3.5 Budgets

Budgets are enforced by the runtime, not requested in a prompt:

```toml
[budgets]
max_tool_calls = 40            # executions per run
max_model_turns = 12           # model round-trips per run
max_runtime_seconds = 900      # wall clock per run
max_patch_bytes = 512000       # largest patch accepted
max_command_output_bytes = 65536  # largest command output kept
```

When a budget is hit the run stops with a notice (`Stopped: model turn budget
(12) reached.`) and the work log records what happened.

---

## 4. Configuration reference

`rai config --template` prints a commented file. Every key:

```toml
[model]
provider = "local"        # openai | local | scripted
model = "local-heuristic" # provider-specific model id
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"   # name of the env var, never a literal key
max_output_tokens = 2048
temperature = 0.0
stream = true
# script = "./scripts/edit-demo.json"   # required when provider = "scripted"

[workspace]
root = "."                      # relative to the directory holding .rai/
allow_writes = true
allow_absolute_paths = false    # keep false: absolute paths are an escape hatch
max_file_bytes = 1048576
exclude = ["target", "node_modules", "dist", "build", ".venv", "venv", "__pycache__", ".git", ".rai"]

[commands]
auto_allow = ["cargo check", "cargo test", "cargo fmt --check", "cargo clippy", "cargo build", "git status", "git diff"]
deny = ["git push", "git reset --hard", "git clean", "rm -rf", "shutdown"]
timeout_seconds = 300
max_output_bytes = 65536
allowed_env = []                # extra environment variables for children
allow_shell = false             # argv execution preferred; shell is opt-in
redact_output = true

[policy]
approval = "prompt"             # prompt | deny | auto
auto_approve = ["ReadOnly"]
allow_destructive = false
allow_credential_sensitive = false

[budgets]
max_tool_calls = 40
max_model_turns = 12
max_runtime_seconds = 900
max_patch_bytes = 512000
max_command_output_bytes = 65536

[search]
prefer_ripgrep = true
max_results = 50
max_indexed_files = 20000

# [[mcp.servers]]
# name = "git"
# transport = "stdio"
# command = "uvx"
# args = ["mcp-server-git"]
# enabled = true
# timeout_seconds = 30
# [mcp.servers.env]
# GIT_DIR = "."
```

Unknown keys are rejected with a precise error, so a typo cannot silently
disable a guard:

```text
rai: error: configuration is not valid: invalid config value `aproval` in /p/.rai/config.toml: unknown field `aproval`, expected one of ...
```

`rai config --check` validates and prints the effective settings, including the
per-mode tool surface:

```text
configuration is valid
config file     /your-project/.rai/config.toml
project root    /your-project
workspace root  /your-project
model           provider=local model=local-heuristic base_url=https://api.openai.com/v1 stream=true
writes          allow_writes=true allow_absolute_paths=false max_file_bytes=1048576
approval        mode=prompt auto_approve=[ReadOnly]
commands        timeout=300s max_output=65536 shell=false auto_allow=[cargo check; cargo test; ...]
                deny=[git push; git reset --hard; ...]
budgets         tool_calls=40 model_turns=12 runtime=900s patch_bytes=512000
search          ripgrep=true max_results=50
mcp servers     0

tools[ask]      explain_failure (ReadOnly), fetch_docs (Network), git_diff (ReadOnly), git_status (ReadOnly), list_files (ReadOnly), list_symbols (ReadOnly), read_file (ReadOnly), search_text (ReadOnly), summarize_diff (ReadOnly)

tools[edit]      apply_patch (WorkspaceWrite), explain_failure (ReadOnly), ... read_file (ReadOnly), search_text (ReadOnly), summarize_diff (ReadOnly)

tools[review]      explain_failure (ReadOnly), ... search_text (ReadOnly), summarize_diff (ReadOnly)

tools[edit+commands] apply_patch (WorkspaceWrite), ..., run_command (Command), run_tests (Command), ...
```

### Environment overrides

Useful for CI or one-off runs. They apply after the file is loaded.

| Variable | Effect |
| --- | --- |
| `RAI_PROVIDER` | overrides `[model].provider` |
| `RAI_MODEL` | overrides `[model].model` |
| `RAI_BASE_URL` | overrides `[model].base_url` |
| `RAI_API_KEY_ENV` | overrides the env-var *name* holding the key |
| `RAI_APPROVAL` | overrides `[policy].approval` |
| `RAI_WORKSPACE` | overrides `[workspace].root` |
| `RAI_SCRIPT` | overrides `[model].script` |
| `OPENAI_API_KEY` (by default) | the credential itself; never logged, never sent to a child process |

---

## 6. Command reference

Every command accepts `--config <PATH>`, `--json` (JSONL events), and `--quiet`.
`--config` may point at a config file or at the project directory that owns `.rai/`.

### `rai init`

Writes `.rai/config.toml` and seeds `.rai/memories.md` from the repository
(detected test/format/lint commands, layout).

```bash
rai init                 # refuses to overwrite an existing config
rai init --force         # rewrite it
```

### `rai index`

Builds the local index: paths, languages, symbol-like declarations, byte sizes,
and the git summary. The model never receives the repository itself, only the
index summary and the results of tool calls.

```bash
rai index --stats
```

```text
[context]  indexing /path/to/project
[notice]   indexed 3 file(s), 5 symbol(s), 0 skipped as binary
files      3
bytes      361
symbols    5
binary     0
truncated  false
languages
  rust           2
  toml           1
```

`target/`, `node_modules/`, `.git/`, `.rai/`, and anything in
`[workspace].exclude` are skipped. Search uses ripgrep when it is on `PATH` and
falls back to a built-in scanner when it is not.

### `rai config`

```bash
rai config --check        # validate, then print the effective trust model
rai config --template     # print the commented template
```

```text
configuration is valid
config file     /path/to/project/.rai/config.toml
model           provider=local model=local-heuristic base_url=https://api.openai.com/v1 stream=true
writes          allow_writes=true allow_absolute_paths=false max_file_bytes=1048576
approval        mode=prompt auto_approve=[ReadOnly]
commands        timeout=300s max_output=65536 shell=false auto_allow=[cargo check; ...]
budgets         tool_calls=40 model_turns=12 runtime=900s patch_bytes=512000

tools[ask]      explain_failure (ReadOnly), fetch_docs (Network), git_diff (ReadOnly), ...
tools[edit]     apply_patch (WorkspaceWrite), explain_failure (ReadOnly), ...
tools[edit+commands] apply_patch (WorkspaceWrite), ..., run_command (Command), run_tests (Command), ...
```

That listing *is* the capability report: it shows exactly which tools each mode
can reach, with their risk classes.

### `rai ask`

```bash
rai ask "where is the retry limit set?"
rai ask "how does policy evaluate a call?" --provider openai --model gpt-5-mini
rai ask "..." --max-tool-calls 6 --json
```

Ask mode may only reach read-only tools (plus `fetch_docs`). It cannot modify the
workspace and cannot run commands.

### `rai edit`

```bash
rai edit "add a JSON export to the report generator"
rai edit "..." --dry-run          # validate patches, write nothing
rai edit "..." --verify           # run inferred verification commands afterwards
rai edit "..." --allow-commands   # let the model run allowlisted commands to check its work
rai edit "..." --yes              # approve writes without prompting
```

Writes happen only through `apply_patch`. `--verify` runs the cheapest
verification command first (`cargo check`, then `cargo test` for Rust; the
project's own command otherwise) and, if it fails, feeds a structured failure
report back to the model for another attempt, bounded by
`[budgets].max_model_turns`.

With the offline `local` provider, `edit` accepts three request forms:

```bash
# 1. an explicit unified diff
rai edit '--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
-let x = 1;
+let x = 2;' --yes

# 2. a targeted replacement
rai edit 'replace in src/lib.rs: "let limit = 3;" -> "let limit = 5;"' --yes

# 3. a new file
rai edit 'create file docs/notes.md: <<<line one\nline two>>>' --yes
```

The local provider cannot invent code. For real edits, point `[model].provider`
at a hosted or local model.

### `rai review`

Reviews the git diff without needing a model. Deterministic rules flag added
credentials, lockfile changes, CI changes, `unsafe`, new `unwrap`, removed
tests, large deletions, and added TODOs.

```bash
rai review                 # worktree diff
rai review --staged        # index diff
rai review --narrative     # add a model-written narrative on top
```

Exit code `0` when nothing high-severity was found, `3` when something was, so
it works as a pre-commit or CI gate:

```text
$ rai review

risk scan       1 changed file(s) (worktree)
  src/lib.rs                               +2 -0
  [high] possible-secret    src/lib.rs - added a value that looks like a credential: const KEY: &str = "sk-abc...";
[exit 3]
```

### `rai run`

Runs one command through the command sandbox. This is the only path that
executes something with full output capture.

```bash
rai run cargo check                  # auto_allow match, runs directly
rai run cargo test --lib
rai run --shell "cargo test && cargo clippy"    # requires allow_shell = true
rai run --cwd crates/api cargo test
rai run --timeout 600 cargo build --release
```

Rules:

- `argv` execution by default; a single string with spaces is split for you.
- Shell metacharacters require an explicit `--shell`, otherwise the command is
  refused with instructions.
- `[commands].deny` is checked first and always wins over `auto_allow`.
- The child's exit code becomes `rai`'s exit code.
- Working directory stays inside the workspace; `--cwd` cannot escape it.

```text
$ rai run cargo check
[command]  cargo check (command matches [commands].auto_allow entry `cargo check`)
[out:stderr]    Checking demo v0.1.0
[out:stderr]    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.09s
[command]  cargo check -> exit 0 in 120ms (stdout 0 B, stderr 142 B)
```

### `rai mcp`

Consume MCP servers configured under `[[mcp.servers]]`:

```toml
[[mcp.servers]]
name = "filesystem"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "."]
enabled = true
timeout_seconds = 30
```

```bash
rai mcp list-tools
rai mcp list-resources
rai mcp call filesystem.read --json-args '{"path":"src/main.rs"}'
```

`mcp call` goes through the same policy engine as everything else: a discovered
MCP tool is treated as `Network` risk, so it needs `--yes` or an entry in
`[policy].auto_approve`.

### `rai mcp-serve`

Serves `rai` itself as an MCP server on stdin/stdout, exposing six narrow tools:

| Tool | Maps to |
| --- | --- |
| `project.search` | `search_text` |
| `project.read_file` | `read_file` |
| `project.apply_patch` | `apply_patch` |
| `project.run_tests` | `run_tests` |
| `project.summarize_diff` | `summarize_diff` |
| `project.explain_failure` | `explain_failure` |

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"demo","version":"0"}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"project.search","arguments":{"query":"retries"}}}' \
  | rai mcp-serve
```

```json
{"jsonrpc":"2.0","id":2,"result":{"content":[{"text":"engine: ripgrep\nmatch_count: 3\n---\nsrc/lib.rs:2:     pub retries: u32,\n...","type":"text"}],"isError":false,"structuredContent":{"engine":"ripgrep","matches":3,"truncated":false}}}
```

An external client gets the same policy decisions as the CLI. Interactive
approval is impossible over a protocol stream, so `[policy].approval = "prompt"`
is downgraded to `deny` with a warning on stderr; writes then require
`auto_approve`.

### `rai memory`

Project memory lives in `.rai/memories.md`, is human-editable, and is injected
into every prompt.

```bash
rai memory add commands "cargo nextest run"
rai memory list
rai memory forget "cargo nextest run"
```

### `rai sessions`

Every run writes an append-only JSONL transcript.

```bash
rai sessions list
rai sessions show latest          # summarized event log
rai sessions show latest --raw    # the JSONL itself
rai sessions compact latest --limit-bytes 200000
```

```text
id                         mode        bytes events  msgs  task
run-187e051b0000           run           646      2     0  cargo check
edit-187e04a50000          edit         4720     15     6  replace in src/lib.rs: "let limit = 3;" -> "let limit = 5;"
ask-187e044c0000           ask          3730     12     5  where is the retry limit set?
```

`compact` drops streamed deltas, truncates long tool results, and keeps the
factual work log (phases, applied patches, approvals, command outcomes, errors).
The uncompacted file is preserved as `<id>.full.jsonl`. Resume an interrupted run
with `--resume <id>`.

---

## 7. Patch format

Unified diffs only. `apply_patch` is the single write path, so this is the one
format worth getting right.

```diff
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -6,6 +6,6 @@
     pub fn new() -> Self {
-        let limit = 3;
+        let limit = 5;
         Self { retries: limit }
     }
 }
```

| Case | How to write it |
| --- | --- |
| Modify a file | `--- a/path` and `+++ b/path` |
| Create a file | `--- /dev/null`, `+++ b/path`, hunk `@@ -0,0 +1,N @@` |
| Delete a file | `--- a/path`, `+++ /dev/null` |
| Rename | Not supported: delete plus create |

Each hunk line starts with exactly one marker: a space (context), `-` (remove),
or `+` (add).

The applier is deliberately forgiving about three mistakes that models make
constantly, and counts each repair in the `patch` event:

1. **Drifted line numbers.** Only the hunk *body* is used to find the anchor;
   the numbers in `@@ ... @@` are advisory. A hunk is applied wherever its
   context+removed lines actually occur, searched outward from the stated
   position.
2. **Stripped context lines.** A blank line inside a hunk is read as an empty
   context line rather than ending the hunk.
3. **Line-number gutters.** If a model pastes back the numbered file view
   (`  12 |     let x = 1;`), the `NNN | ` gutter is stripped.

What it will not do: touch a path outside the workspace, write a non-UTF-8 file,
exceed `[workspace].max_file_bytes` or `[budgets].max_patch_bytes`, or apply a
hunk whose context does not exist. A failure is returned to the model as text,
so it can correct itself - and the runtime stops the run if the same unchanged
call fails twice.

---

## 8. Safety guarantees

These are enforced in Rust, not requested in a prompt:

| Guarantee | Where |
| --- | --- |
| `ask` and `review` cannot write or run anything | `tools::tool_available`, `apply_patch` mode check |
| Every path stays inside `[workspace].root` | `util::safe_join`, rejects `..`, absolute paths, symlink escapes |
| Binary and oversized files are refused | `index::read_text_file` |
| Only patches write files, atomically | `patch::apply` writes to a temp file, then renames |
| Commands get a filtered environment | `sandbox::env_allowed`; secret-shaped names never pass |
| Commands time out and are killed with their children | `sandbox::run` (`taskkill /T` on Windows) |
| Output is capped, and truncation is reported | `[commands].max_output_bytes` |
| Secrets are masked in output, logs, and transcripts | `redact::Redactor` over env values and token patterns |
| A refused call cannot be repeated forever | `agent::MAX_IDENTICAL_UNPRODUCTIVE_CALLS` |
| Tool calls, model turns, runtime, and patch size are all bounded | `[budgets]` |
| Everything is written to an append-only transcript | `.rai/sessions/<id>.jsonl` |

What `rai` deliberately does **not** do:

- read or write outside the workspace by default (`[workspace].allow_absolute_paths` exists but is off);
- run a shell unless `--shell` is passed *and* `[commands].allow_shell = true`;
- delete files, rewrite git history, or push (no `Destructive` tool ships in this build);
- add found code to the index or send it anywhere except the configured model endpoint;
- silently grow its own capabilities. Every new class of action is a config change.

---

## 9. Troubleshooting

**`provider \`openai\` needs $OPENAI_API_KEY to be set`**
Set the variable named by `[model].api_key_env`, or run offline with
`--provider local`, or point `[model].base_url` at a local server (Ollama on
`http://localhost:11434/v1`, LM Studio on `http://localhost:1234/v1`).

**`tool \`apply_patch\` was not executed: needs approval`**
The default is `approval = "prompt"` and a non-interactive shell cannot answer.
Either pass `--yes` for that run, or record the decision in configuration
(`approval = "auto"` plus `auto_approve = ["WorkspaceWrite"]`). This is the
intended behaviour, not a bug.

**`hunk ... did not match the file contents`**
The context lines in the patch do not exist as written. Read the file again
(`rai ask` or a fresh `edit`) and regenerate the hunk; the applier already
tolerates drifted line numbers, stripped context lines, and line-number gutters,
so a mismatch usually means the file changed underneath the patch.

**`Stopped: \`apply_patch\` produced the same refused or failing result ...`**
The runtime's no-progress guard. The model asked for the identical call twice and
was refused or failed both times. Grant the capability deliberately, or explain
the task differently.

**`command ... contains shell metacharacters`**
Split the arguments (`rai run cargo test --lib`) or opt in explicitly:
`rai run --shell "cargo test && cargo clippy"`, with
`[commands].allow_shell = true` in config.

**Search falls back to the internal scanner**
Install ripgrep and put it on `PATH`; `rai index --stats` and the
`engine:` line in `search_text` results show which engine ran.

**MCP server will not start**
Check the `command`/`args` pair manually first (`npx -y ... --help`) with the same
working directory. Set `enabled = false` to take a broken server out of the
rotation, and raise `timeout_seconds` for slow start-up.

**`[policy].approval = "prompt"` warning from `mcp-serve`**
Expected: an MCP client cannot answer a prompt. The server downgrades to `deny`
rather than reading the protocol stream as an answer.

---

## 10. What was verified

`cargo test` runs 219 tests: 159 unit tests inside the library and 60 integration
tests across six files. `cargo clippy --all-targets` is clean and
`cargo fmt --all -- --check` passes.

| Area | How it is covered |
| --- | --- |
| Patch parsing and application | unit tests for drift, stripped context, gutters, create/delete, path escape, binary and size refusal; integration tests applying patches through the real registry |
| Policy and approvals | decision matrix per risk class; integration tests proving `ask` cannot write, `--yes` can, and destructive actions stay denied |
| Command sandbox | stdout/stderr separation, exit codes, timeout kill, output cap, env filtering (a secret-shaped variable in the parent must not reach the child), streamed chunks, redaction |
| Agent loop | scripted provider drives real tool execution: tool budget enforcement, one result per tool call, refusal handling, no-progress stop, cancellation, `--verify` |
| Providers | OpenAI-compatible adapter exercised against a local socket server speaking SSE, including streamed tool-call fragments and usage; offline `local` and `scripted` providers have their own tests |
| MCP | the crate's own client against the real `rai mcp-serve` binary: handshake, `tools/list`, search, read, patch, refused patch, `run_tests`, `explain_failure`, unknown-tool protocol error |
| CLI | `init`, `config --check`, `config --template`, `index --stats`, `memory` round-trip, `ask --provider local` plus session recording, `sessions list/show`, `review` exit codes, JSONL output shape, `run` allowlist/shell/exit codes |

A full worked example, run against a throwaway crate:

```text
$ rai init
wrote .rai/config.toml
wrote .rai/memories.md

$ rai ask "where is the retry limit set?" --provider local
[context]  indexing /path/to/demo
[notice]   indexed 3 file(s), 5 symbol(s), 0 skipped as binary
[session]  ask (answer questions using repository search and file reads) provider=local model=local-heuristic session=ask-187e044c0000
[model]    turn 1 via local (local-heuristic)
Offline planner: gathering context for `where is the retry limit set?`.
[approval] search_text (ReadOnly) -> allowed: search_text is read-only
[tool]     search_text (ReadOnly) (max_results=30, query=limit|retry|set)
[tool]     search_text ok in 11ms - 2 match(es) for `limit|retry|set`
[approval] list_files (ReadOnly) -> allowed: list_files is read-only
[tool]     list_files (ReadOnly) (limit=60)
[tool]     list_files ok in 0ms - listed 3 path(s)
[model]    turn 2 via local (local-heuristic)
Answer from the local heuristic provider (no LLM configured).
It retrieved and ranked repository matches; it does not reason about them.

query: where is the retry limit set?

--- search ---
engine: ripgrep
match_count: 2
---
src/lib.rs:7:         let limit = 3;
src/lib.rs:8:         Self { retries: limit }

--- files ---
3 file(s) matched, showing 3
Cargo.toml	toml	59 bytes
src/errors.rs	rust	63 bytes
src/lib.rs	rust	239 bytes

Set [model].provider = "openai" with a real endpoint for synthesized answers.

[done]     ask complete: 2 tool call(s), 0 file(s) changed, 0 command(s), 15ms

$ rai edit 'replace in src/lib.rs: "let limit = 3;" -> "let limit = 5;"' --yes
[tool]     read_file ok in 0ms - read src/lib.rs lines 1-14
Offline planner: replacing "let limit = 3;" with "let limit = 5;" in src/lib.rs.
[approval] apply_patch (WorkspaceWrite) -> requested: apply_patch has risk class WorkspaceWrite and is not auto-approved
[approval] apply_patch (WorkspaceWrite) -> allowed: approved by --yes
[patch]    applied 1 file(s), +1 -1
[tool]     apply_patch ok in 60ms - applied 1 file(s), +1 -1
Offline planner: the patch was already applied.
[done]     edit complete: 2 tool call(s), 1 file(s) changed, 0 command(s), 72ms

$ rai run cargo check
[command]  cargo check (command matches [commands].auto_allow entry `cargo check`)
[command]  cargo check -> exit 0 in 120ms (stdout 0 B, stderr 142 B)

$ rai review
risk scan       1 changed file(s) (worktree)
  src/lib.rs                               +1 -1
  no rule-based risks found
[exit 0]

$ printf 'const KEY: &str = "sk-abc...";\n' >> src/lib.rs && rai review
risk scan       1 changed file(s) (worktree)
  src/lib.rs                               +3 -1
  [high] possible-secret    src/lib.rs - added a value that looks like a credential
[exit 3]
```

Not verified here: a live call to a hosted model endpoint (no key was available
in this environment). The adapter that would make it is covered by the local
socket-server tests above, and `rai ask --provider openai` fails fast with a
clear message when the configured credential is missing.


