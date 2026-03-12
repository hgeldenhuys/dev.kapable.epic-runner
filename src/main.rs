use clap::Parser;
use epic_runner::commands::Commands;
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Parser)]
#[command(
    name = "epic-runner",
    version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("GIT_HASH"), ")"),
    about = "Epic-scoped autonomous sprint execution"
)]
pub struct Cli {
    #[arg(long, global = true, env = "KAPABLE_API_URL")]
    pub url: Option<String>,

    #[arg(long, global = true)]
    pub key: Option<String>,

    #[arg(long, global = true, default_value = "false")]
    pub json: bool,

    #[arg(long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[tokio::main]
async fn main() {
    // Ignore SIGPIPE so piped output (e.g., `| head -10`) doesn't kill the process tree.
    // Without this, a broken pipe from a truncated consumer sends SIGPIPE to the entire
    // process group, killing orchestrate + sprint-run mid-flight.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }

    let cli = Cli::parse();

    // Initialise structured logging. Honour RUST_LOG; fall back to
    // warn (quiet) or debug (--verbose).
    let default_level = if cli.verbose { "debug" } else { "warn" };
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    fmt::Subscriber::builder()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
    let client = match epic_runner::api_client::ApiClient::from_env_with_overrides(
        cli.url.clone(),
        cli.key.clone(),
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };
    let config = epic_runner::commands::CliConfig {
        json: cli.json,
        verbose: cli.verbose,
    };
    if let Err(e) = epic_runner::commands::run(cli.command, &client, &config).await {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
