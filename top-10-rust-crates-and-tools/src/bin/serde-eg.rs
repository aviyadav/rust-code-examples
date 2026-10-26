use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
struct Order {
    id: u64,
    customer_email: String,
    total_cents: u32,
    #[serde(default)]
    notes: Option<String>,
    #[serde(rename = "createdAt")]
    created_at: String,
}

fn main() -> Result<(), serde_json::Error> {
    let raw_json = r#"{
        "id": 2,
        "customer_email": "corro@example.com",
        "total_cents": 1000,
        "notes": "the unsafe one",
        "createdAt": "2023-01-01T00:00:00Z"
    }"#;

    let order: Order = serde_json::from_str(raw_json)?;
    println!("{}", serde_json::to_string_pretty(&order)?);

    Ok(())
}
