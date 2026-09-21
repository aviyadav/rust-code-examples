use std::sync::mpsc;
use std::thread;
use std::time::Duration;
fn main() {
    println!("📧 Starting channel communication example...");
    // Concept: We're setting up a new multi-producer, single-consumer channel here
    let (tx, rx) = mpsc::channel(); // Gives us our Sender (tx) and Receiver (rx)
    let mut handles = vec![]; // To keep track of our worker threads
    for i in 0..3 {
        let thread_tx = mpsc::Sender::clone(&tx); // Use Case: Each worker gets its own cloned sender!
        let handle = thread::spawn(move || {
            let data = format!("Result-{}", i); // Our worker makes some data
            println!("Worker {} sending: {}", i, data);
            thread_tx.send(data).unwrap(); // ZAP! Send that data through the channel
            thread::sleep(Duration::from_millis(50)); // A tiny pause, you know, for dramatic effect
        });
        handles.push(handle);
    }
    // IMPORTANT: Drop the original sender *here*. I mean, this is super important.
    // It's how the receiver knows that, eventually, no more messages are coming once all the cloned senders are also gone.
    // Seriously, without this, the `for received in rx` loop might just sit there forever!
    drop(tx);
    println!("Main thread waiting for messages...");
    for received in rx {
        // Use Case: And here's our main thread, just chilling, collecting all the messages
        println!("Main thread received: {}", received);
    }
    for handle in handles {
        handle.join().unwrap(); // Waiting for all those sender threads to finish up
    }
    println!("✅ Channel communication example finished!");
}

