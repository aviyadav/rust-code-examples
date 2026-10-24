//! Logging events to stdout or a file with the custom subscriber framework.
//!
//! [`emit_events`] is the shared event source. [`log_to_stdout`] and
//! [`log_to_file`] install the same [`CustomSubscriber`] with different
//! [`Write`] destinations, so the emitting code does not know where its events
//! end up.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use tracing::Level;

use crate::{CustomSubscriber, OutputFormat, SubscriberConfig};

/// Emits a small mixed-level workload, including a span with context.
pub fn emit_events() {
    tracing::info!(service = "checkout", version = "1.0.0", "service starting");

    let request = tracing::info_span!("request", request_id = "req-42", user = "ada");
    let _guard = request.enter();

    tracing::debug!(items = 2, "processing cart");
    tracing::info!(order_id = "ORD-2001", total_cents = 6496, "order accepted");
    tracing::warn!(stock = 3, "low stock");
    tracing::error!(code = 500, "downstream timeout");
}

/// Logs [`emit_events`] to standard output.
pub fn log_to_stdout() {
    log_to_writer(io::stdout());
}

/// Logs [`emit_events`] to `path`, creating parent directories if needed.
///
/// The file is created or truncated. Writes are flushed after every event by
/// [`CustomSubscriber`].
pub fn log_to_file(path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }

    let file = File::create(path)?;
    log_to_writer(file);
    Ok(())
}

/// Logs [`emit_events`] to any writer that can be owned by the subscriber.
pub fn log_to_writer<W>(writer: W)
where
    W: Write + Send + 'static,
{
    let subscriber = CustomSubscriber::with_writer(config(), writer);
    tracing::subscriber::with_default(subscriber, emit_events);
}

fn config() -> SubscriberConfig {
    SubscriberConfig {
        default_level: Level::DEBUG,
        module_filters: Vec::new(),
        format: OutputFormat::Text,
        include_timestamps: true,
        include_spans: true,
    }
}
