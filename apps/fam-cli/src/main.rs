mod commands;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "fam",
    version,
    about = "FAM resource manager: name search, tags and change tracking"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    /// Configuration file; omitted keys keep their defaults.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Override the tag database location from configuration.
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    #[arg(long, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Inspect or create configuration.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Enumerate files without a query.
    Scan {
        dir: Option<PathBuf>,
        #[arg(long)]
        limit: Option<usize>,
        /// Restrict to one display bucket, for example image or document.
        #[arg(long)]
        kind: Option<String>,
    },
    /// Search file names; every term must appear somewhere in the name.
    Search {
        query: String,
        #[arg(long = "in")]
        within: Option<PathBuf>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        /// walk (always available) or everything (index backend).
        #[arg(long)]
        backend: Option<String>,
    },
    /// Manage tags on a single file.
    Tag {
        #[command(subcommand)]
        action: TagAction,
    },
    /// Find files by tag.
    Find {
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Require every listed tag instead of any of them.
        #[arg(long)]
        all: bool,
        #[arg(long = "in")]
        within: Option<PathBuf>,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        offset: Option<usize>,
    },
    /// Watch directories and print merged change batches.
    Watch {
        dirs: Vec<PathBuf>,
        #[arg(long, default_value_t = 10)]
        seconds: u64,
    },
    /// Report database contents.
    Stats,
    /// Manage the credentials an out-of-process agent presents.
    ///
    /// This is the only place a grant is made or taken away, and it is a command a person runs:
    /// neither the ABI nor the MCP surface can reach it.
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
    /// Read the audit log.
    ///
    /// The core never exposes this over the ABI, which is the point: whatever can ask the tool for
    /// files must not also be the only thing that can read what it asked for.
    Audit {
        #[command(subcommand)]
        action: AuditAction,
    },
}

#[derive(Subcommand)]
enum McpAction {
    /// Issue, inspect, revoke and rotate agent credentials.
    Token {
        #[command(subcommand)]
        action: TokenAction,
    },
}

#[derive(Subcommand)]
enum TokenAction {
    /// Issue a credential. The secret is printed once and never stored in a readable form.
    Add {
        /// The name the audit trail will show. Letters, digits, dot, dash, underscore.
        label: String,
        /// browse, edit or admin.
        #[arg(long, default_value = "browse")]
        tier: String,
        /// A directory the token may reach; repeatable. Only `admin` may pass `*`.
        #[arg(long = "workspace")]
        workspace: Vec<String>,
    },
    /// Every credential on file, with what each one is allowed to do.
    List,
    /// Retire a credential. Its row stays, so the trail keeps resolving its label.
    Revoke { id: String },
    /// New secret, same identity and same policy.
    Rotate { id: String },
}

#[derive(Subcommand)]
enum AuditAction {
    /// Print the last lines, oldest first.
    Tail {
        #[arg(long, short = 'n', default_value_t = 50)]
        lines: usize,
    },
    /// Recompute every hash link and session seal. Exits non-zero when anything disagrees.
    Verify,
    /// Sizes, counts, and which paths the log talks about most.
    Stats {
        #[arg(long, default_value_t = 10)]
        top: usize,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Print the configuration file location.
    Path,
    /// Print the effective configuration.
    Show,
    /// Write a template configuration file.
    Init {
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum TagAction {
    Add {
        path: PathBuf,
        tags: Vec<String>,
        #[arg(long, default_value = "manual")]
        source: String,
        #[arg(long, default_value_t = 0)]
        priority: i32,
    },
    Remove {
        path: PathBuf,
        tags: Vec<String>,
        #[arg(long)]
        source: Option<String>,
    },
    List {
        path: PathBuf,
        #[arg(long)]
        source: Option<String>,
    },
    Rename {
        path: PathBuf,
        from: String,
        to: String,
        #[arg(long)]
        source: Option<String>,
    },
    Priority {
        path: PathBuf,
        tag: String,
        priority: i32,
        #[arg(long, default_value = "manual")]
        source: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn init_logging(verbose: bool) {
    let level = if verbose { "debug" } else { "warn" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level))
        .format_target(false)
        .init();
}

fn run(cli: &Cli) -> anyhow::Result<()> {
    let mut config = match &cli.config {
        Some(path) => fam_core::Config::load_merging(path)?,
        None => fam_core::Config::load(),
    };
    if let Some(db) = &cli.db {
        config.database_path = db.clone();
    }
    config.validate()?;

    match &cli.command {
        Commands::Config { action } => commands::config(&config, action),
        Commands::Scan { dir, limit, kind } => {
            commands::scan(&config, dir.clone(), *limit, kind.as_deref())
        }
        Commands::Search {
            query,
            within,
            kind,
            limit,
            backend,
        } => commands::search(
            &config,
            query,
            within.clone(),
            kind.as_deref(),
            *limit,
            backend.as_deref(),
        ),
        Commands::Tag { action } => commands::tag(&config, action),
        Commands::Find {
            tags,
            all,
            within,
            source,
            limit,
            offset,
        } => commands::find(
            &config,
            tags,
            *all,
            within.clone(),
            source.as_deref(),
            *limit,
            *offset,
        ),
        Commands::Watch { dirs, seconds } => commands::watch(&config, dirs, *seconds),
        Commands::Stats => commands::stats(&config),
        Commands::Mcp { action } => commands::mcp(&config, action),
        Commands::Audit { action } => commands::audit(&config, action),
    }
}
