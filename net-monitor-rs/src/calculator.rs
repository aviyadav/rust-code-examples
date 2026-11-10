use crate::collector::InterfaceSnapshot;
use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, Clone, Default)]
pub struct InterfaceRate {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_rate: f64, // Bytes per second
    pub tx_rate: f64, // Bytes per second
    pub rx_pps: f64,  // Packets per second
    pub tx_pps: f64,  // Packets per second
    pub rx_errors: u64,
    pub tx_errors: u64,
    pub rx_dropped: u64,
    pub tx_dropped: u64,
}

pub struct StateManager {
    previous: HashMap<String, InterfaceSnapshot>,
    previous_time: Instant,
    pub current_rates: Vec<InterfaceRate>,
}

impl StateManager {
    pub fn new() -> Self {
        Self {
            previous: HashMap::new(),
            previous_time: Instant::now(),
            current_rates: Vec::new(),
        }
    }

    pub fn update(&mut self, current: HashMap<String, InterfaceSnapshot>) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.previous_time).as_secs_f64();
        let mut new_rates = Vec::new();

        // Only calculate rates if we have a previous baseline and time has passed
        if elapsed > 0.0 && !self.previous.is_empty() {
            for (name, stats) in &current {
                if let Some(old) = self.previous.get(name) {
                    // saturating_sub prevents negative rates if a counter resets
                    let rx_diff = stats.rx_bytes.saturating_sub(old.rx_bytes);
                    let tx_diff = stats.tx_bytes.saturating_sub(old.tx_bytes);
                    let rx_pkt_diff = stats.rx_packets.saturating_sub(old.rx_packets);
                    let tx_pkt_diff = stats.tx_packets.saturating_sub(old.tx_packets);

                    new_rates.push(InterfaceRate {
                        name: name.clone(),
                        rx_bytes: stats.rx_bytes,
                        tx_bytes: stats.tx_bytes,
                        rx_rate: rx_diff as f64 / elapsed,
                        tx_rate: tx_diff as f64 / elapsed,
                        rx_pps: rx_pkt_diff as f64 / elapsed,
                        tx_pps: tx_pkt_diff as f64 / elapsed,
                        rx_errors: stats.rx_errors,
                        tx_errors: stats.tx_errors,
                        rx_dropped: stats.rx_dropped,
                        tx_dropped: stats.tx_dropped,
                    });
                }
            }
        }

        // Sort by interface name for a consistent UI layout
        new_rates.sort_by(|a, b| a.name.cmp(&b.name));

        self.current_rates = new_rates;
        self.previous = current;
        self.previous_time = now;
    }
}
