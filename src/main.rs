use semmem::cli::Command;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let cli = semmem::cli::parse_cli();

    match cli.command {
        Command::RUN {
            data_dir,
            listen,
            issuer,
            audience,
            public_url,
        } => semmem::server::run(data_dir, cli.socket, listen, issuer, audience, public_url).await,
        _ => semmem::cli::execute(cli).await,
    }
}
