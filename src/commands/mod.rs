pub mod backlog;
pub mod epic;
pub mod flow_validate;
pub mod impediment;
pub mod init;
pub mod orchestrate;
pub mod product;
pub mod research;
pub mod retro;
pub mod review;
pub mod run_sprint;
pub mod sprint;
pub mod status;

use crate::api_client::ApiClient;
use clap::Subcommand;

pub struct CliConfig {
    pub json: bool,
    pub verbose: bool,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize epic-runner project (provision Kapable project + tables)
    Init(init::InitArgs),
    /// Manage products
    Product(product::ProductArgs),
    /// Manage backlog stories
    Backlog(backlog::BacklogArgs),
    /// Manage epics
    Epic(epic::EpicArgs),
    /// Manage sprints
    Sprint(sprint::SprintArgs),
    /// Orchestrate an epic (thin supervisor — spawns sprint processes)
    Orchestrate(orchestrate::OrchestrateArgs),
    /// Execute a single sprint's ceremonies (fat executor — called by orchestrate)
    #[command(name = "sprint-run")]
    SprintRun(run_sprint::SprintRunArgs),
    /// Run business review ceremony (standalone)
    Review(review::ReviewArgs),
    /// Run retrospective ceremony (standalone)
    Retro(retro::RetroArgs),
    /// Manage impediments (cross-epic blockers)
    Impediment(impediment::ImpedimentArgs),
    /// Validate a ceremony flow YAML file
    #[command(name = "flow-validate")]
    FlowValidate(flow_validate::FlowValidateArgs),
    /// Manage research notes (add, list, show, link, unlink)
    Research(research::ResearchArgs),
    /// Show dashboard status
    Status(status::StatusArgs),
}

pub async fn run(
    command: Commands,
    client: &ApiClient,
    cli: &CliConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        Commands::Init(args) => init::run(args, client, cli).await,
        Commands::Product(args) => product::run(args, client, cli).await,
        Commands::Backlog(args) => backlog::run(args, client, cli).await,
        Commands::Epic(args) => epic::run(args, client, cli).await,
        Commands::Sprint(args) => sprint::run(args, client, cli).await,
        Commands::Orchestrate(args) => orchestrate::run(args, client, cli).await,
        Commands::SprintRun(args) => run_sprint::run(args, client, cli).await,
        Commands::Review(args) => review::run(args, client, cli).await,
        Commands::Retro(args) => retro::run(args, client, cli).await,
        Commands::Impediment(args) => impediment::run(args, client, cli).await,
        Commands::FlowValidate(args) => Ok(flow_validate::run(args)?),
        Commands::Research(args) => research::run(args, client, cli).await,
        Commands::Status(args) => status::run(args, client, cli).await,
    }
}
