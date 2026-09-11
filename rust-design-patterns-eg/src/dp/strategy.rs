// First up, define our Strategy trait
trait CompressionStrategy {
    fn compress(&self, data: &[u8]) -> Vec<u8>;
}
// Here's our first Concrete Strategy: Kinda like Gzip
struct GzipCompression;
impl CompressionStrategy for GzipCompression {
    fn compress(&self, data: &[u8]) -> Vec<u8> {
        println!("Compressing with Gzip...");
        // In a real project, you'd totally use a real Gzip library here!
        data.iter().map(|&b| b.saturating_add(1)).collect() // This is just a pretend "compression"
    }
}
// And here's Concrete Strategy B: More like Lz4
struct Lz4Compression;
impl CompressionStrategy for Lz4Compression {
    fn compress(&self, data: &[u8]) -> Vec<u8> {
        println!("Compressing with Lz4...");
        // Again, a real Lz4 library would go here in production!
        data.iter().map(|&b| b.saturating_add(2)).collect() // Another pretend "compression"
    }
}
// This is our Context struct, it's the one that uses a strategy
struct DataProcessor {
    strategy: Box<dyn CompressionStrategy>,
}
impl DataProcessor {
    fn new(strategy: Box<dyn CompressionStrategy>) -> Self {
        DataProcessor { strategy }
    }
    fn process_data(&self, data: &[u8]) -> Vec<u8> {
        self.strategy.compress(data)
    }
}
fn main() {
    let original_data = b"Hello, Rust strategies!";
    // Let's try the Gzip strategy first
    let gzip_processor = DataProcessor::new(Box::new(GzipCompression));
    let compressed_with_gzip = gzip_processor.process_data(original_data);
    println!("Gzip compressed data (example): {:?}", compressed_with_gzip);
    println!("---");
    // Now, let's switch to the Lz4 strategy
    let lz4_processor = DataProcessor::new(Box::new(Lz4Compression));
    let compressed_with_lz4 = lz4_processor.process_data(original_data);
    println!("Lz4 compressed data (example): {:?}", compressed_with_lz4);
}

