# packet-analyzer

A lightweight command-line network traffic analyzer written in Rust. It captures live TCP/UDP packets from a network interface, parses their headers, and reports traffic statistics — total packets and bytes, unique connections, and the top talkers on the network.

## Features

- **Live packet capture** on the system's default network interface via [libpcap](https://www.tcpdump.org/)
- **Header-only capture** (`snaplen 96`) — payloads are never read or stored
- **BPF filtering** — only TCP and UDP traffic is captured (`tcp or udp`)
- **Connection tracking** — counts bytes per unique 5-tuple (src/dst IP, src/dst port, protocol)
- **Per-host byte accounting** — top 10 hosts by traffic volume
- **Graceful shutdown** — Ctrl+C prints a final summary instead of killing the process
- **Periodic reporting** — summary printed every 10 seconds

## How it works

Each captured frame is parsed in pure Rust, no external parsing crates:

```
Ethernet frame ──► (VLAN tags skipped) ──► IPv4 header ──► TCP/UDP ports
```

- Non-IPv4, non-TCP/UDP, and malformed/truncated frames are skipped safely
- Byte counts use the IP header's **total-length field**, so statistics reflect actual wire traffic even though only headers are captured
- Packets are aggregated into a `Stats` structure keyed by connection and host

## Requirements

- **Rust** 1.85+ (uses edition 2024)
- **libpcap development files**

| Distro | Install command |
|---|---|
| Debian / Ubuntu | `sudo apt install libpcap-dev` |
| Fedora / RHEL | `sudo dnf install libpcap-devel` |
| Arch | `sudo pacman -S libpcap` |

## Building

```sh
cargo build --release
```

## Usage

Capturing on a live interface requires elevated privileges (`CAP_NET_RAW` / `CAP_NET_ADMIN`). Either run with `sudo`:

```sh
sudo cargo run
```

Or grant the built binary the needed capabilities once, so it can run unprivileged:

```sh
sudo setcap cap_net_raw,cap_net_admin=eip target/release/packet-analyzer
./target/release/packet-analyzer
```

Press `Ctrl+C` to stop capture and print the final summary.

### Example output

```
--- traffic summary ---
packets 1423  bytes 1876420  connections 38
  192.168.1.42: 921340 bytes
  142.250.185.78: 402113 bytes
  ...
```

## Dependencies

| Crate | Version | Purpose |
|---|---|---|
| `pcap` | 2.5.0 | Safe bindings to libpcap for packet capture |
| `ctrlc` | 3.5.2 | Ctrl+C signal handling for graceful shutdown |

## Project structure

```
packet-analyzer/
├── Cargo.toml        # package metadata and dependencies
└── src/
    └── main.rs       # capture loop, packet parsing, and stats
```
