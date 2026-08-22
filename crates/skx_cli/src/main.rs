use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

mod commands;

#[derive(Parser)]
#[command(
    name = "skx",
    version,
    about = "The universal AI skill manager, cross-agent compiler, and TUI cockpit"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Force a colour theme for the TUI, overriding config and detection.
    ///
    /// Set `NO_COLOR` in the environment to disable colour entirely.
    #[arg(long, global = true, value_name = "light|dark|auto")]
    theme: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Detect active agents in the current directory and create skx.toml.
    Init,
    /// Install a skill from a local path (Git/registry URLs: not yet supported).
    #[command(alias = "a")]
    Add {
        source: String,
        #[arg(short = 'g', long)]
        global: bool,
        #[arg(long = "agent")]
        agents: Vec<String>,
    },
    /// Cleanly remove a skill and unlink across all target agent configs.
    #[command(alias = "rm")]
    Remove { name: String },
    /// Report SKILL.md files on disk that aren't tracked by skx yet
    /// (report-only — use `skx tui` and press 'd' to actually import).
    Discover {
        /// Extra directories to scan recursively, beyond the global caches
        /// and this workspace's own .claude/skills / .agents/skills.
        paths: Vec<String>,
    },
    /// List installed skills, their scopes, and target links.
    #[command(alias = "ls")]
    List,
    /// Reconcile symlinks, compile outputs, and fix configuration drift.
    #[command(alias = "s")]
    Sync,
    /// Check for conflicting triggers, broken symlinks, or missing MCP servers.
    Audit,
    /// Compile canonical skills into standalone static files.
    #[command(alias = "ex")]
    Export {
        /// Directory to write the exported files into.
        #[arg(short = 'o', long, default_value = "skx-export")]
        out: String,
    },
    /// Open the full-screen interactive TUI dashboard.
    Tui,
}

fn home_dir() -> Result<std::path::PathBuf> {
    skx_core::home_dir().context("could not determine the current user's home directory")
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    let root = std::env::current_dir().context("could not determine the current directory")?;

    match cli.command {
        Some(Command::Init) => commands::init(&root),
        Some(Command::Add {
            source,
            global,
            agents,
        }) => {
            if !agents.is_empty() {
                eprintln!(
                    "note: --agent is not yet used to filter targets; every target the skill declares will be compiled on `skx sync`"
                );
            }
            commands::add(&root, &home_dir()?, &source, global)
        }
        Some(Command::Remove { name }) => commands::remove(&root, &home_dir()?, &name),
        Some(Command::Discover { paths }) => {
            let extra_roots: Vec<std::path::PathBuf> =
                paths.into_iter().map(std::path::PathBuf::from).collect();
            commands::discover(&root, &home_dir()?, &extra_roots)
        }
        Some(Command::List) => commands::list(&root),
        Some(Command::Sync) => commands::sync(&root, &home_dir()?),
        Some(Command::Audit) => commands::audit(&root, &home_dir()?),
        Some(Command::Export { out }) => {
            commands::export(&root, &home_dir()?, std::path::Path::new(&out))
        }
        Some(Command::Tui) | None => skx_tui::run(cli.theme.as_deref()),
    }
}
