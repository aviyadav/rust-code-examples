# profile-check

A small deployment profile validator CLI in Rust built with **Clap**, **Miette**, and **Tracing**:

- **Clap** (`clap` with `derive`): owns command-line syntax, help text, flags, and subcommands.
- **Miette** (`miette` with `fancy`): owns user-fixable configuration diagnostics with source locations, highlighted spans, and repair guidance.
- **Tracing** (`tracing` + `tracing-subscriber` with `env-filter`): owns structured execution logs controlled via `--verbose` or `RUST_LOG`.

## Project Structure

```text
src/
  main.rs                 # Parse CLI, initialize tracing, dispatch commands
  cli.rs                  # Clap structs and subcommands
  commands/
    mod.rs
    validate.rs           # `validate` command orchestration
  profile.rs              # Profile parsing, validation rules, and unit tests
  diagnostics.rs          # Reusable source-aware Miette diagnostics
samples/
  deploy.env              # Valid production profile
  staging-extra.env       # Valid staging profile with an unknown key
  bad-port.env            # Invalid port value
  bad-env.env             # Unsupported environment value
  missing-equals.env      # Missing `=` in entry
  missing-port.env        # Missing required `port` key
```

## Build & Test

```bash
# Build the debug binary
cargo build

# Run unit tests
cargo test
```

## Usage

```bash
# Show top-level help or subcommand help
cargo run -- --help
cargo run -- validate --help
```

### Validating Profiles

```bash
# Valid profile
cargo run -- validate samples/deploy.env

# Enable debug logs with --verbose
cargo run -- --verbose validate samples/deploy.env

# Control log filtering with RUST_LOG (Linux/macOS - bash/zsh)
RUST_LOG=profile_check=debug cargo run -- validate samples/deploy.env --strict

# Control log filtering with RUST_LOG (Windows - PowerShell)
$env:RUST_LOG="profile_check=debug"; cargo run -- validate samples/deploy.env --strict

# Unknown keys warn by default, but fail (exit code 1) under --strict
cargo run -- validate samples/staging-extra.env
cargo run -- validate samples/staging-extra.env --strict
```

### Error & Diagnostic Examples

These examples intentionally fail validation or CLI parsing (exiting with a non-zero exit code) to demonstrate Miette diagnostics and Clap usage errors. When run via `cargo run`, Cargo also prints `error: process didn't exit successfully`.

```bash
# Invalid port (Miette diagnostic)
cargo run -- validate samples/bad-port.env

# Unsupported environment (Miette diagnostic)
cargo run -- validate samples/bad-env.env

# Missing `=` separator (Miette diagnostic)
cargo run -- validate samples/missing-equals.env

# Missing required setting (Miette diagnostic)
cargo run -- validate samples/missing-port.env

# Missing CLI argument (Clap usage error)
cargo run -- validate
```

You can also invoke the compiled binary directly via `./target/debug/profile-check` (or `.\target\debug\profile-check.exe` on Windows) after running `cargo build`.
