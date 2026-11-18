use clap::Parser;

#[derive(Parser)]
#[command(name = "worker", about = "Channel delivery worker (push|sms|email)")]
struct Args {
    #[arg(long)]
    channel: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    if !common::valid_channel(&args.channel) {
        anyhow::bail!("--channel must be push, sms, or email");
    }
    engine::worker::run(engine::worker::WorkerConfig::from_env(&args.channel)).await
}