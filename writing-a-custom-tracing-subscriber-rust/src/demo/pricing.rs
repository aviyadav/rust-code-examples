use super::model::Item;

/// Orders above this total are flagged for manual review.
pub const HIGH_VALUE_CENTS: u64 = 10_000;

pub fn total_cents(items: &[Item]) -> u64 {
    items
        .iter()
        .map(|item| item.unit_price_cents * u64::from(item.quantity))
        .sum()
}

pub fn check_threshold(total_cents: u64) {
    tracing::debug!(
        total_cents,
        threshold_cents = HIGH_VALUE_CENTS,
        "pricing calculated"
    );

    if total_cents > HIGH_VALUE_CENTS {
        tracing::warn!(
            total_cents,
            threshold_cents = HIGH_VALUE_CENTS,
            "high-value order flagged for review"
        );
    }
}
