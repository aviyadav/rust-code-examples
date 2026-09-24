use std::process;

fn main() {
    if let Err(error) = zero_copy_log_parser::run_generator() {
        eprintln!("error: {error}");
        process::exit(1);
    }
}