use std::time::Duration;

#[derive(Debug)]
struct Order {
    id: u32,
    total_cents: u32,
}

async fn fetch_order(id: u32) -> Result<Order, String> {
    tokio::time::sleep(Duration::from_millis(100 * id as u64)).await;
    Ok(Order {
        id,
        total_cents: id * 1000,
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let handles: Vec<_> = (1..=3)
        .map(|id| tokio::spawn(async move { fetch_order(id).await }))
        .collect();

    for handle in handles {
        let order = handle.await??;
        println!(
            "Order #{}: ${:.2}",
            order.id,
            order.total_cents as f64 / 100.0
        );
    }

    Ok(())
}
