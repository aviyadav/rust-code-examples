use std::sync::{Arc, Mutex};
use std::thread;
fn main() {
    println!("🔢 Starting shared counter example...");
    // Concept: Arc for sharing, Mutex for "only one at a time" access. Our precious counter!
    let counter = Arc::new(Mutex::new(0)); // This is our shared, super-protected counter
    let mut handles = vec![]; // We'll keep track of our threads here
    for i in 0..10 {
        // Use Case: Cloning the Arc, so each thread gets its own "key" to the counter
        let counter_clone = Arc::clone(&counter);
        let handle = thread::spawn(move || {
            let mut num = counter_clone.lock().unwrap(); // Gotta grab that lock! This waits if it's busy.
            *num += 1; // Okay, now we can safely change the number!
            println!("Thread {} incremented counter to {}", i, *num);
            // Psst! The lock automatically lets go when `num` goes out of scope. Clever, right?
        });
        handles.push(handle);
    }
    for handle in handles {
        handle.join().unwrap(); // Waiting for all our little helper threads to finish
    }
    // Now we can finally see the grand total after everyone's done their part
    println!("Final counter value: {}", *counter.lock().unwrap());
    println!("✅ Shared counter example finished!");
}
