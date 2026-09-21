use std::{thread, time::Duration};

fn main() {
    println!("⭐ Main thread starting!");
    // Concept: Spawning a new thread, like, for real!
    let handle = thread::spawn(|| {
        for i in 1..=5 {
            println!("👋 Hello from spawned thread: {}", i);
            thread::sleep(Duration::from_millis(1));
        }
    });

    for i in 1..=3 {
        println!("🚀 Hello from main thread: {}", i);
        thread::sleep(Duration::from_millis(1));
    }

    // Use Case : Gotta wait for the little guy to finish before we wrap up!
    println!("Waiting for spawned thread to finish...");
    handle.join().unwrap();
    println!("✅ Main thread finished!");
}
