mod cache;
mod db;
mod http;
mod model;
mod noisy;
mod payments;
mod pricing;

use custom_tracing_subscriber::{CustomSubscriber, OutputFormat, SubscriberConfig};
use tracing::Level;

use model::{Order, PaymentStatus};

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let orders = model::load_orders()?;

    eprintln!(
        "Loaded {} synthetic orders from data/orders.json (embedded at compile time).",
        orders.len()
    );

    eprintln!("\n=== 1/3 text output: default INFO, db=TRACE, cache=DEBUG, noisy=WARN ===");
    tracing::subscriber::with_default(CustomSubscriber::new(text_config()), || {
        process_orders(&orders);
    });

    eprintln!("\n=== 2/3 JSON output: same filters, one JSON object per line ===");
    tracing::subscriber::with_default(CustomSubscriber::new(json_config()), || {
        process_orders(&orders);
    });

    eprintln!("\n=== 3/3 text output filtered to WARN and ERROR: only issues are shown ===");
    tracing::subscriber::with_default(CustomSubscriber::with_default_level(Level::WARN), || {
        process_orders(&orders)
    });

    let declined = orders
        .iter()
        .filter(|order| order.payment.status == PaymentStatus::Declined)
        .count();
    let review = orders
        .iter()
        .filter(|order| order.payment.status == PaymentStatus::Review)
        .count();
    let high_value = orders
        .iter()
        .filter(|order| pricing::total_cents(&order.items) > pricing::HIGH_VALUE_CENTS)
        .count();
    let slow = orders
        .iter()
        .filter(|order| order.db_latency_ms > db::SLOW_QUERY_MS)
        .count();

    eprintln!(
        "\nProcessed {} orders: {} declined, {} held for review, {} high-value, {} slow queries.",
        orders.len(),
        declined,
        review,
        high_value,
        slow
    );

    Ok(())
}

fn text_config() -> SubscriberConfig {
    SubscriberConfig {
        default_level: Level::INFO,
        module_filters: module_filters(),
        format: OutputFormat::Text,
        include_timestamps: true,
        include_spans: true,
    }
}

fn json_config() -> SubscriberConfig {
    SubscriberConfig {
        default_level: Level::INFO,
        module_filters: module_filters(),
        format: OutputFormat::Json,
        include_timestamps: true,
        include_spans: true,
    }
}

fn module_filters() -> Vec<(String, Level)> {
    let demo = module_path!();
    vec![
        (format!("{demo}::db"), Level::TRACE),
        (format!("{demo}::cache"), Level::DEBUG),
        (format!("{demo}::noisy"), Level::WARN),
    ]
}

fn process_orders(orders: &[Order]) {
    for order in orders {
        process_order(order);
    }
}

fn process_order(order: &Order) {
    let checkout = tracing::info_span!(
        "checkout",
        order_id = %order.order_id,
        customer = %order.customer.name,
        customer_id = %order.customer.id,
        tier = %order.customer.tier,
        channel = %order.channel,
        total_cents = tracing::field::Empty,
        cache_hit = tracing::field::Empty,
    );
    let _checkout_guard = checkout.enter();

    http::receive(order);
    cache::lookup(order);

    let items = db::load_items(order);
    let total_cents = pricing::total_cents(items);
    checkout.record("total_cents", total_cents);
    checkout.record("cache_hit", order.cache_hit);

    pricing::check_threshold(total_cents);
    payments::charge(order, total_cents);
    http::respond(order, total_cents);
    noisy::emit(order);
}
