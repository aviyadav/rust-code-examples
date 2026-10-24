use std::error::Error;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Order {
    pub order_id: String,
    pub customer: Customer,
    pub channel: String,
    pub items: Vec<Item>,
    pub payment: Payment,
    pub cache_hit: bool,
    pub db_latency_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Customer {
    pub id: String,
    pub name: String,
    pub tier: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Item {
    pub sku: String,
    pub quantity: u32,
    pub unit_price_cents: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Payment {
    pub method: String,
    pub status: PaymentStatus,
    pub reference: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentStatus {
    Approved,
    Declined,
    Review,
}

pub fn load_orders() -> Result<Vec<Order>, Box<dyn Error>> {
    // Embedded so the binary works from any working directory.
    let orders = include_str!("../../data/orders.json");
    Ok(serde_json::from_str(orders)?)
}
