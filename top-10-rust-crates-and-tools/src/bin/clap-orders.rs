use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;

/// Manage orders from the command line
#[derive(Parser, Debug)]
#[command(name = "orders", version, about, long_about = None)]
struct Cli {
    /// Increase logging verbosity (-v, -vv, -vvv)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Output format
    #[arg(short, long, value_enum, default_value_t = Format::Table, global = true)]
    format: Format,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// List orders, optionally filtered
    List(ListArgs),

    /// Show a single order by ID
    Show {
        /// Order ID
        id: u64,
    },

    /// Create a new order
    Create {
        /// Customer email address
        #[arg(long)]
        email: String,

        /// Order total in dollars, e.g. 19.99
        #[arg(long, value_parser = parse_dollars)]
        total: u32,

        /// Optional note attached to the order
        #[arg(long)]
        note: Option<String>,
    },
}

#[derive(Args, Debug)]
struct ListArgs {
    /// Only show orders with this status
    #[arg(short, long, value_enum)]
    status: Option<Status>,

    /// Maximum number of orders to show
    #[arg(short, long, default_value_t = 10, value_parser = clap::value_parser!(u16).range(1..=100))]
    limit: u16,

    /// Only show orders for this customer email
    #[arg(long)]
    customer: Option<String>,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum Format {
    Table,
    Json,
}

#[derive(ValueEnum, Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Status {
    Pending,
    Shipped,
    Cancelled,
}

#[derive(Serialize, Debug)]
struct Order {
    id: u64,
    customer_email: String,
    total_cents: u32,
    status: Status,
    note: Option<String>,
}

/// Parses "19.99" or "20" into cents, rejecting bad input with a helpful message.
fn parse_dollars(s: &str) -> Result<u32, String> {
    let dollars: f64 = s
        .parse()
        .map_err(|_| format!("`{s}` is not a valid amount"))?;
    if dollars <= 0.0 {
        return Err("total must be greater than 0".into());
    }
    Ok((dollars * 100.0).round() as u32)
}

fn sample_orders() -> Vec<Order> {
    let order = |id, email: &str, total_cents, status| Order {
        id,
        customer_email: email.into(),
        total_cents,
        status,
        note: None,
    };
    vec![
        order(1, "ferris@rust-lang.org", 1999, Status::Shipped),
        order(2, "corro@example.com", 4550, Status::Pending),
        order(3, "ferris@rust-lang.org", 1200, Status::Cancelled),
        order(4, "alice@example.com", 8999, Status::Pending),
        order(5, "bob@example.com", 2500, Status::Shipped),
    ]
}

fn print_orders(orders: &[&Order], format: Format) {
    match format {
        Format::Json => println!("{}", serde_json::to_string_pretty(orders).unwrap()),
        Format::Table => {
            println!(
                "{:<4} {:<24} {:>10}  {:<10}",
                "ID", "CUSTOMER", "TOTAL", "STATUS"
            );
            for o in orders {
                println!(
                    "{:<4} {:<24} {:>10}  {:<10}",
                    o.id,
                    o.customer_email,
                    format!("${:.2}", o.total_cents as f64 / 100.0),
                    format!("{:?}", o.status),
                );
            }
        }
    }
}

fn main() {
    let cli = Cli::parse();

    if cli.verbose > 0 {
        eprintln!("[debug] verbosity level {}: {:?}", cli.verbose, cli.command);
    }

    let orders = sample_orders();

    match cli.command {
        Command::List(args) => {
            let matches: Vec<&Order> = orders
                .iter()
                .filter(|o| args.status.is_none_or(|s| o.status == s))
                .filter(|o| {
                    args.customer
                        .as_ref()
                        .is_none_or(|c| &o.customer_email == c)
                })
                .take(args.limit as usize)
                .collect();
            print_orders(&matches, cli.format);
        }
        Command::Show { id } => match orders.iter().find(|o| o.id == id) {
            Some(order) => print_orders(&[order], cli.format),
            None => {
                eprintln!("error: no order with ID {id}");
                std::process::exit(1);
            }
        },
        Command::Create { email, total, note } => {
            let order = Order {
                id: orders.len() as u64 + 1,
                customer_email: email,
                total_cents: total,
                status: Status::Pending,
                note,
            };
            println!("Created order #{}", order.id);
            print_orders(&[&order], cli.format);
        }
    }
}
