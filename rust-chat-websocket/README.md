# Rust Chat WebSocket

A small real-time chat application with an **Axum WebSocket server** written in Rust and a dependency-free **browser client** served by Bun.

Every connected client receives every text message, including messages it sends itself. Messages exist only in memory and are not saved.

## Features

- Real-time messaging between multiple browser windows or WebSocket clients
- Concurrent sending and receiving with Tokio
- Broadcast channel shared by all connected clients
- Simple responsive browser interface
- Connection status and basic error feedback
- Graceful server shutdown with `Ctrl+C`
- No JavaScript packages or build step required

## Requirements

- [Rust and Cargo](https://rustup.rs/) with Rust 1.85 or newer (the project uses Rust edition 2024)
- [Bun](https://bun.sh/) for serving the browser client
- A modern browser with WebSocket support

Bun is only used to serve `index.html`; it does not replace the Rust chat server.

## Quick start

Run all commands from the project root.

### 1. Start the Rust server

```sh
cargo run
```

Cargo downloads and builds the Rust dependencies on the first run. When the server is ready, it prints:

```text
Rust chat server listening on 127.0.0.1:3000
```

Keep this terminal open.

### 2. Start the browser client

Open a second terminal in the project root and run:

```sh
bun --port 3001 index.html
```

No `bun install` or other installation step is needed.

### 3. Chat between two windows

Open <http://localhost:3001> in two browser windows or tabs. Wait until both show **Connected**, type a message, and press **Enter** or click **Send**.

The message should appear in both windows.

## WebSocket endpoint

```text
ws://127.0.0.1:3000/ws
```

`ws://localhost:3000/ws` is equivalent when connecting from the same machine.

The endpoint accepts an HTTP `GET` request with a WebSocket upgrade. It handles **text messages**; binary and other non-text messages are ignored.

### Connect with websocat

[websocat](https://github.com/vi/websocat) is an optional command-line WebSocket client. Install it with:

```sh
cargo install websocat
```

With the Rust server running, connect using:

```sh
websocat ws://127.0.0.1:3000/ws
```

Type a message and press Enter. Run the same command in another terminal to chat between terminal clients, or leave a browser client open to chat between the browser and terminal.

### Connect from browser JavaScript

```js
const socket = new WebSocket("ws://127.0.0.1:3000/ws");

socket.onopen = () => socket.send("Hello!");
socket.onmessage = (event) => console.log(event.data);
```

## How it works

1. Axum listens on `127.0.0.1:3000` and upgrades requests to `/ws` into WebSocket connections.
2. The server creates a Tokio broadcast channel with capacity for 256 messages.
3. Each connection subscribes to that channel when it joins.
4. Incoming text is sent to the broadcast channel and relayed to every subscriber, including the sender.
5. `tokio::select!` lets each connection process incoming and outgoing messages concurrently.
6. Closing a socket or encountering a socket error ends that connection's handler without stopping the other clients.

The browser uses standard DOM and WebSocket APIs. Messages are rendered with `textContent`, so received text is not interpreted as HTML.

## Project structure

```text
.
├── src/
│   └── main.rs       # Axum routes, WebSocket handling, and broadcasting
├── index.html        # Browser interface and WebSocket client
├── Cargo.toml        # Rust package metadata and dependencies
├── Cargo.lock        # Locked Rust dependency versions
└── README.md
```

### Main Rust dependencies

- **Axum** — HTTP routing, server integration, and WebSocket support
- **Tokio** — asynchronous runtime, networking, broadcast channels, and shutdown signals

## Configuration

The current addresses are hardcoded for local development:

| Setting | Value | Location |
| --- | --- | --- |
| Rust server address | `127.0.0.1:3000` | `src/main.rs` |
| WebSocket route | `/ws` | `src/main.rs` |
| Browser WebSocket URL | `ws://127.0.0.1:3000/ws` | `index.html` |
| Browser development port | `3001` | The `bun` command |
| Broadcast channel capacity | 256 messages | `src/main.rs` |

If you change the Rust server address or route, also update the WebSocket URL in `index.html`. To use another port for the browser client, change only the Bun command, for example:

```sh
bun --port 3002 index.html
```

## Development commands

```sh
# Check the Rust code without producing a final binary
cargo check

# Build a debug binary
cargo build

# Build an optimized release binary
cargo build --release

# Run the server
cargo run

# Run Rust tests, if any are added
cargo test
```

Stop either server by focusing its terminal and pressing `Ctrl+C`.

## Limitations

This is a local demo, not a production chat service:

- There are no usernames, accounts, authentication, private messages, or chat rooms.
- Messages are not persisted; new clients cannot see earlier messages.
- The server listens only on the local machine (`127.0.0.1`).
- Connections use unencrypted `ws://`, not `wss://`.
- There are no message-size limits, rate limits, or moderation controls.
- A sufficiently slow client can miss messages when the bounded broadcast buffer is overwritten.

## Troubleshooting

### The browser shows a connection error

Confirm that `cargo run` is still running and shows the listening message. The browser client must connect to the Rust server on port **3000**, not the Bun server on port **3001**.

### Port 3000 or 3001 is already in use

Stop the process already using that port. For the browser client, you can instead choose another port in the Bun command. Changing the Rust port also requires updating the browser's WebSocket URL.

### A new window does not show old messages

This is expected: the application broadcasts live messages only and does not store chat history.

### The sender sees its own message

This is intentional. Every message is broadcast to all connected clients, including its sender.
