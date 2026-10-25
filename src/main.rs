use clap::Parser;

use semantic_memory::cli::{Cli, Command};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();

    match cli.command {
        Command::SERVE {
            data_dir,
            listen,
            issuer,
            audience,
            public_url,
        } => {
            semantic_memory::server::run(data_dir, cli.socket, listen, issuer, audience, public_url)
                .await
        }
        _ => semantic_memory::cli::execute(cli).await,
    }
}
