fn main() {
    let numbers = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    // So, we'll filter the even ones, double them, then sum the total
    let sum_of_doubled_evens: u32 = numbers
        .iter() // First, make an iterator over our numbers
        .filter(|&n| n % 2 == 0) // Only let the even numbers through, please!
        .map(|&n| n * 2) // Now, take each of those and double it
        .sum(); // And finally, add 'em all up!
    println!("Original numbers: {:?}", numbers);
    println!("Sum of doubled even numbers: {}", sum_of_doubled_evens);
    // Output: Sum of doubled even numbers: 60 (2*2 + 4*2 + 6*2 + 8*2 + 10*2 = 4 + 8 + 12 + 16 + 20 = 60)
    // BTW, making your own custom iterators is pretty simple, too!
    struct MyRange {
        current: u32,
        end: u32,
    }
    impl Iterator for MyRange {
        type Item = u32;
        fn next(&mut self) -> Option<Self::Item> {
            if self.current < self.end {
                let result = self.current;
                self.current += 1;
                Some(result)
            } else {
                None
            }
        }
    }
    let my_iterator = MyRange { current: 1, end: 5 };
    println!("Custom range iterator sum: {}", my_iterator.sum::<u32>());
    // Output: Custom range iterator sum: 10 (1+2+3+4 = 10)
}
