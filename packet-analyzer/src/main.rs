use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use pcap::{Capture, Device};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Protocol {
    Tcp,
    Udp,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Connection {
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    protocol: Protocol,
}

#[derive(Default)]
struct Stats {
    connections: HashMap<Connection, u64>,
    bytes_per_host: HashMap<Ipv4Addr, u64>,
    total_packets: u64,
    total_bytes: u64,
}

impl Stats {
    fn record(&mut self, conn: Connection, bytes: u64) {
        *self.bytes_per_host.entry(conn.src_ip).or_default() += bytes;
        *self.bytes_per_host.entry(conn.dst_ip).or_default() += bytes;
        *self.connections.entry(conn).or_default() += bytes;
        self.total_packets += 1;
        self.total_bytes += bytes;
    }

    fn print_summary(&self) {
        println!("\n--- traffic summary ---");
        println!(
            "packets {}  bytes {}  connections {}",
            self.total_packets,
            self.total_bytes,
            self.connections.len()
        );

        let mut hosts: Vec<_> = self.bytes_per_host.iter().collect();
        hosts.sort_by_key(|(_, bytes)| std::cmp::Reverse(**bytes));
        for (host, bytes) in hosts.iter().take(10) {
            println!("  {host}: {bytes} bytes");
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let running = Arc::new(AtomicBool::new(true));
    let flag = running.clone();
    ctrlc::set_handler(move || flag.store(false, Ordering::SeqCst))?;

    let device = Device::lookup()?.ok_or("no device found")?;
    let mut cap = Capture::from_device(device)?
        .promisc(true)
        .snaplen(96) // headers only: we never look at payload
        .timeout(1000)
        .open()?;
    cap.filter("tcp or udp", true)?;

    let mut stats = Stats::default();
    let mut last_print = Instant::now();

    while running.load(Ordering::SeqCst) {
        match cap.next_packet() {
            Ok(packet) => {
                if let Some((conn, bytes)) = parse_packet(packet.data) {
                    stats.record(conn, bytes);
                }
            }
            Err(pcap::Error::TimeoutExpired) => {}
            Err(e) => {
                eprintln!("capture error: {e}");
                break;
            }
        }

        if last_print.elapsed() >= Duration::from_secs(10) {
            stats.print_summary();
            last_print = Instant::now();
        }
    }

    stats.print_summary();
    Ok(())
}

fn parse_packet(data: &[u8]) -> Option<(Connection, u64)> {
    // Ethernet frame: 14-byte header, ethertype at offset 12
    if data.len() < 14 {
        return None;
    }
    let mut ethertype = u16::from_be_bytes([data[12], data[13]]);
    let mut offset = 14;
    // Skip 802.1Q / 802.1ad VLAN tags
    while ethertype == 0x8100 || ethertype == 0x88a8 {
        if data.len() < offset + 4 {
            return None;
        }
        ethertype = u16::from_be_bytes([data[offset + 2], data[offset + 3]]);
        offset += 4;
    }
    if ethertype != 0x0800 {
        return None; // not IPv4
    }

    // IPv4 header
    let ip = &data[offset..];
    if ip.len() < 20 {
        return None;
    }
    let version_ihl = ip[0];
    if version_ihl >> 4 != 4 {
        return None;
    }
    let ihl = (version_ihl & 0x0f) as usize;
    if ihl < 5 {
        return None;
    }
    let ip_hdr_len = ihl * 4;
    if ip.len() < ip_hdr_len {
        return None;
    }
    let total_len = u16::from_be_bytes([ip[2], ip[3]]) as usize;
    let src_ip = Ipv4Addr::new(ip[12], ip[13], ip[14], ip[15]);
    let dst_ip = Ipv4Addr::new(ip[16], ip[17], ip[18], ip[19]);

    // L4 header: src/dst ports are the first 4 bytes for both TCP and UDP
    let l4 = &ip[ip_hdr_len..];
    if l4.len() < 4 {
        return None;
    }
    let src_port = u16::from_be_bytes([l4[0], l4[1]]);
    let dst_port = u16::from_be_bytes([l4[2], l4[3]]);
    let protocol = match ip[9] {
        6 => Protocol::Tcp,
        17 => Protocol::Udp,
        _ => return None,
    };

    // snaplen truncates the frame, so use the IP total-length field for
    // byte accounting; fall back to the captured length if it looks bogus
    let bytes = if total_len >= ip_hdr_len + 4 {
        total_len as u64
    } else {
        data.len() as u64
    };

    Some((
        Connection {
            src_ip,
            dst_ip,
            src_port,
            dst_port,
            protocol,
        },
        bytes,
    ))
}
