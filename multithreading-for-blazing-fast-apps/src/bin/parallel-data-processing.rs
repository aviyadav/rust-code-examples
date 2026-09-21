use rayon::prelude::*; // Concept: Gotta bring in Rayon's parallel iterator magic!
use std::time::Instant;
fn heavy_computation(n: u64) -> u64 {
    // Just pretending to do some really hard CPU-intensive stuff here
    let mut sum = 0;
    for _ in 0..1_000_000 {
        // Loop a million times, just for fun
        sum += n;
    }
    sum
}
fn main() {
    println!("⚡ Starting Rayon parallel processing example...");
    let numbers: Vec<u64> = (1..=20).collect(); // A vector of numbers, you know, our test data
    // Use Case: First, let's see how slow it is the old-fashioned way
    let start_sequential = Instant::now();
    let sequential_results: Vec<u64> = numbers
        .iter() // Just a regular old iterator
        .map(|&num| heavy_computation(num))
        .collect();
    let duration_sequential = start_sequential.elapsed();
    println!(
        "Sequential results (first 5): {:?}",
        &sequential_results[0..5]
    );
    println!("Sequential processing took: {:?}", duration_sequential);
    // Use Case: Now for the good stuff - parallel processing with Rayon!
    let start_parallel = Instant::now();
    let parallel_results: Vec<u64> = numbers
        .par_iter() // Concept: BAM! Just change .iter() to .par_iter() - that's it!
        .map(|&num| heavy_computation(num))
        .collect();
    let duration_parallel = start_parallel.elapsed();
    println!("Parallel results (first 5): {:?}", &parallel_results[0..5]);
    println!("Parallel processing took: {:?}", duration_parallel);
    println!("✅ Rayon parallel processing example finished!");
    // Okay, little thought experiment time...
    println!(
        "\n🤔 Did you even *notice* the performance difference between the sequential and parallel versions? I bet you did! The more cores your machine has, the bigger that impact gets. It's truly something."
    );
}

