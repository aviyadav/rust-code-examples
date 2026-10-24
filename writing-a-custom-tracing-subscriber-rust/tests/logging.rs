use std::fs;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use custom_tracing_subscriber::logging::{log_to_file, log_to_writer};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Capture {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).expect("log output should be UTF-8")
    }
}

#[test]
fn writer_receives_all_levels_and_span_context() {
    let capture = Capture::default();

    log_to_writer(capture.clone());

    let text = capture.text();
    assert!(text.contains("[DEBUG]"), "{text}");
    assert!(text.contains("[INFO ]"), "{text}");
    assert!(text.contains("[WARN ]"), "{text}");
    assert!(text.contains("[ERROR]"), "{text}");
    assert!(text.contains("service starting"), "{text}");
    assert!(
        text.contains("request{request_id=req-42, user=ada}:"),
        "{text}"
    );
    assert!(text.contains("order accepted"), "{text}");
    assert!(text.contains("downstream timeout"), "{text}");
}

#[test]
fn file_destination_creates_parents_and_writes_events() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-logs")
        .join(format!("run-{}-{stamp}", std::process::id()));
    let path = dir.join("nested/events.log");

    log_to_file(&path).expect("log file should be written");

    let text = fs::read_to_string(&path).expect("log file should be readable");
    assert!(text.contains("service starting"), "{text}");
    assert!(
        text.contains("request{request_id=req-42, user=ada}:"),
        "{text}"
    );
    assert!(text.contains("downstream timeout"), "{text}");

    fs::remove_dir_all(&dir).ok();
}
