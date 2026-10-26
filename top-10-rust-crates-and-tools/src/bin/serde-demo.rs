use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct Address {
    city: String,
    country: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct User {
    id: u32,
    name: String,
    #[serde(rename = "emailAddress")]
    email: String,
    #[serde(default)]
    active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    nickname: Option<String>,
    tags: Vec<String>,
    address: Address,
}

fn main() -> Result<(), serde_json::Error> {
    // Rust struct -> JSON
    let user = User {
        id: 1,
        name: "Ferris".into(),
        email: "ferris@rust-lang.org".into(),
        active: true,
        nickname: None,
        tags: vec!["crab".into(), "rustacean".into()],
        address: Address {
            city: "Berlin".into(),
            country: "Germany".into(),
        },
    };

    let json = serde_json::to_string_pretty(&user)?;
    println!("Serialized:\n{json}\n");

    // JSON -> Rust struct (`active` missing, so it falls back to the default)
    let input = r#"{
        "id": 2,
        "name": "Corro",
        "emailAddress": "corro@example.com",
        "nickname": "the unsafe one",
        "tags": ["mascot"],
        "address": { "city": "Portland", "country": "USA" }
    }"#;

    let parsed: User = serde_json::from_str(input)?;
    println!("Deserialized:\n{parsed:#?}\n");

    // Untyped JSON via serde_json::Value
    let value: serde_json::Value = serde_json::from_str(input)?;
    println!("City from untyped Value: {}", value["address"]["city"]);

    Ok(())
}
