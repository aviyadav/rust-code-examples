# rust-design-patterns-eg

Hands-on examples of classic design patterns and idioms implemented in Rust. Each pattern lives in its own self-contained file under `src/dp/` and runs as an independent binary — no dependencies, no shared code, just the pattern.

## Requirements

- Rust **1.85+** (the crate uses edition 2024 — check with `rustc --version`)

## Quick start

```sh
# Build everything
cargo build

# Run the default binary (src/main.rs)
cargo run

# Run a specific pattern demo
cargo run --bin builder
```

## The patterns

| # | Pattern | File | Run with |
|---|---------|------|----------|
| 1 | Builder | `src/dp/builder.rs` | `cargo run --bin builder` |
| 2 | Newtype | `src/dp/newtype.rs` | `cargo run --bin newtype` |
| 3 | Iterator | `src/dp/iterator.rs` | `cargo run --bin iterator` |
| 4 | Strategy | `src/dp/strategy.rs` | `cargo run --bin strategy` |
| 5 | State | `src/dp/state.rs` | `cargo run --bin state` |
| 6 | Option / Result | `src/dp/option-result.rs` | `cargo run --bin option-result` |
| 7 | Typestate | `src/dp/type-state.rs` | `cargo run --bin type-state` |

### 1. Builder (`src/dp/builder.rs`)

Constructs a complex object (`HttpRequest`) step by step through a fluent, chainable builder (`HttpRequestBuilder`). Each setter consumes and returns `self`, so calls chain naturally, and `build()` performs final validation (the URL must be set) before producing the finished `HttpRequest`.

```sh
cargo run --bin builder
```

```text
Built HTTP Request: HttpRequest { url: "https://api.example.com/data", method: "POST", headers: [("Content-Type", "application/json")], body: Some("{\"key\": \"value\"}"), timeout_ms: 10000 }
```

### 2. Newtype (`src/dp/newtype.rs`)

Wraps primitive types (`u64`) in distinct single-field structs (`UserId`, `ProductId`, `OrderId`) so the compiler can tell them apart. Passing a `ProductId` where a `UserId` is expected is a **compile-time error**, not a runtime bug.

```sh
cargo run --bin newtype
```

```text
Fetching profile for user ID: UserId(12345)
Fetching details for product ID: ProductId(67890)
Order ID: OrderId(98765)
```

### 3. Iterator (`src/dp/iterator.rs`)

Two sides of the iterator pattern:

- **Consuming** built-in iterators: `filter` → `map` → `sum` over a vector (sums the doubled even numbers of `1..=10` → `60`).
- **Implementing** the `Iterator` trait: a custom `MyRange` struct with a `next()` method, summed to `10`.

```sh
cargo run --bin iterator
```

```text
Original numbers: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
Sum of doubled even numbers: 60
Custom range iterator sum: 10
```

### 4. Strategy (`src/dp/strategy.rs`)

Defines a family of interchangeable algorithms behind a trait (`CompressionStrategy`) and injects the chosen one into a context struct (`DataProcessor`) as a `Box<dyn CompressionStrategy>`. Swapping `GzipCompression` for `Lz4Compression` changes behavior without touching the context.

```sh
cargo run --bin strategy
```

```text
Compressing with Gzip...
Gzip compressed data (example): [73, 102, 109, ...]
---
Compressing with Lz4...
Lz4 compressed data (example): [74, 103, 110, ...]
```

### 5. State (`src/dp/state.rs`)

A traffic light that cycles Red → Green → Yellow → Red. Each state (`RedLight`, `GreenLight`, `YellowLight`) implements `TrafficLightState`, and `next_state(self: Box<Self>)` **consumes** the current state and returns the next one. The context swaps states via `std::mem::replace`, since a value behind `&mut self` can't be moved out directly.

```sh
cargo run --bin state
```

```text
Light is RED. STOP! 🛑
Changing from Red to Green... 🚦
Light is GREEN. GO! 🟢
Changing from Green to Yellow... 🚦
Light is YELLOW. PREPARE TO STOP! 🟠
Changing from Yellow to Red... 🚦
Light is RED. STOP! 🛑
```

### 6. Option / Result (`src/dp/option-result.rs`)

Rust's idioms for absence and failure:

- `Option<String>` for a user lookup that may find nothing (`get_username_by_id`).
- `Result<u16, String>` for a port parser that can fail twice: the string may not parse, and the port may be a privileged one (< 1024). Chained with `parse` → `map_err` → `and_then`.

```sh
cargo run --bin option-result
```

```text
Found user: Alice
User not found!
---
Successfully parsed port: 8080
Error parsing port: Invalid port format: invalid digit found in string
Error parsing port: Port 80 is outside valid range (1024-65535)
```

### 7. Typestate (`src/dp/type-state.rs`)

Encodes *protocol state in the type system*: each builder stage is a distinct type (`ConfigBuilder` → `ConfigBuilderWithHost` → `ConfigBuilderWithHostAndPort`), and methods only exist on the stage where they're legal. Calling `set_port` before `set_host`, or `build` before both are set, **doesn't compile**. Marker traits (`HasHost`, `HasPort`) document what each stage carries.

```sh
cargo run --bin type-state
```

```text
Host set: localhost
Port set: 8080
Building NetworkConfig... ✅
Final Network Config: NetworkConfig { host: "localhost", port: 8080 }
```

## Adding a new pattern

1. Create `src/dp/<pattern>.rs` with its own `fn main()` demo.
2. Register it in `Cargo.toml`:

   ```toml
   [[bin]]
   name = "<pattern>"
   path = "src/dp/<pattern>.rs"
   ```

3. Run it: `cargo run --bin <pattern>`.

## Project layout

```
.
├── Cargo.toml            # Package manifest; one [[bin]] entry per pattern
├── README.md
└── src/
    ├── main.rs           # Default binary (plain `cargo run`)
    └── dp/               # One self-contained demo per pattern
        ├── builder.rs
        ├── iterator.rs
        ├── newtype.rs
        ├── option-result.rs
        ├── state.rs
        ├── strategy.rs
        └── type-state.rs
```

## Build configuration notes

- `default-run = "rust-design-patterns-eg"` — with multiple binaries, plain `cargo run` would otherwise be ambiguous; it runs `src/main.rs`.
- `publish = false` — this is a local learning repo, not a crates.io package.
- `[profile.release]` enables `lto`, `codegen-units = 1`, and `strip = true` for smaller, faster release binaries (`cargo build --release`).