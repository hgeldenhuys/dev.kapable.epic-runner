use clap::Parser;
use epic_runner::commands::Commands;

#[derive(Parser)]
#[command(
    name = "epic-runner",
    version,
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
    let cli = Cli::parse();
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
