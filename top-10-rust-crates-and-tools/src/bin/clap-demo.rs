use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "orders", version, about = "Query the orders service")]
struct Args {
    /// Order ID to fetch
    id: u64,

    /// Output format
    #[arg(short, long, default_value = "text")]
    format: String,

    /// increase logging verbosity (-v, -vv, -vvv)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

fn main() {
    let args = Args::parse();
    println!("{:?}", args);
}
