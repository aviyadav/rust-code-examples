use anyhow::Result;
use std::collections::HashMap;
use std::fs;

#[derive(Debug, Clone, Default)]
pub struct InterfaceSnapshot {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub rx_errors: u64,
    pub tx_errors: u64,
    pub rx_dropped: u64,
    pub tx_dropped: u64,
}

pub fn read_network_stats() -> Result<HashMap<String, InterfaceSnapshot>> {
    let content = fs::read_to_string("/proc/net/dev")?;
    let mut interfaces = HashMap::new();

    // Skip the first two header lines
    for line in content.lines().skip(2) {
        let Some((name, values)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_string();
        let fields: Vec<u64> = values
            .split_whitespace()
            .filter_map(|v| v.parse().ok())
            .collect();

        // Ensure we have enough fields (16 fields per interface)
        if fields.len() < 16 {
            continue;
        }

        interfaces.insert(
            name,
            InterfaceSnapshot {
                rx_bytes: fields[0],
                rx_packets: fields[1],
                rx_errors: fields[2],
                rx_dropped: fields[3],
                tx_bytes: fields[8],
                tx_packets: fields[9],
                tx_errors: fields[10],
                tx_dropped: fields[11],
            },
        );
    }

    Ok(interfaces)
}
