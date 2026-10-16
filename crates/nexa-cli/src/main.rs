//! `nexa` — the NexaDB binary: server, shell, and process manager.

mod brand;
mod daemon;
mod paths;
mod server;
mod shell;
mod table;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context as _, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use nexa::Nexa;

#[derive(Parser)]
#[command(
    name = "nexa",
    version,
    about = "NexaDB — a strictly-typed graph database with vectors and full-text search, in one binary.",
    long_about = None,
    after_help = "Quick start:\n  nexa init            write an example schema.nql\n  nexa start --apply schema.nql\n  nexa shell\n\nDocs: https://github.com/RatikGambhir/graphdb",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the server in the foreground (Ctrl-C to stop).
    Serve(ServeArgs),
    /// Start the server in the background.
    Start(ServeArgs),
    /// Stop a running server gracefully.
    Stop(CtlArgs),
    /// Stop, then start again.
    Restart(ServeArgs),
    /// Is it running? Shows address, uptime and graph size.
    Status(CtlArgs),
    /// Show the background server's log.
    Logs {
        /// Keep printing new lines.
        #[arg(short, long)]
        follow: bool,
        /// Lines of history to show.
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: usize,
        /// Data directory of the server.
        #[arg(long, env = "NEXA_DATA", value_name = "DIR")]
        data: Option<PathBuf>,
    },
    /// Interactive NQL shell.
    Shell(ConnArgs),
    /// Run one NQL statement and print the result.
    Query {
        /// The NQL to run, e.g. "SELECT name FROM User".
        nql: String,
        /// Parameter as name=value (JSON values allowed), repeatable.
        #[arg(short, long = "param", value_name = "NAME=VALUE")]
        params: Vec<String>,
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        conn: ConnArgs,
    },
    /// Run an .nql script as one transaction.
    Run {
        /// Script file.
        file: PathBuf,
        #[command(flatten)]
        conn: ConnArgs,
    },
    /// Print the schema as NQL.
    Schema(ConnArgs),
    /// Flush everything to disk and compact the database file.
    #[command(alias = "compact")]
    Snapshot(ConnArgs),
    /// Write an example schema.nql to get started.
    Init {
        /// Directory to write into.
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Run NexaDB as a background service that starts at login
    /// (launchd on macOS, systemd on Linux).
    Service {
        #[command(subcommand)]
        action: ServiceCmd,
    },
}

#[derive(Subcommand)]
enum ServiceCmd {
    /// Install and start the service.
    Install(ServeArgs),
    /// Stop and remove the service.
    Uninstall,
    /// Print the service file that `install` would write.
    Print(ServeArgs),
}

#[derive(Copy, Clone, ValueEnum)]
enum Sync {
    /// fsync every commit (safest).
    Always,
    /// let the OS flush (fastest).
    Never,
}

#[derive(Args, Clone)]
struct ServeArgs {
    /// Data directory [default: ~/Library/Application Support/NexaDB on macOS].
    #[arg(long, env = "NEXA_DATA", value_name = "DIR")]
    data: Option<PathBuf>,
    /// Keep everything in memory; nothing is written to disk.
    #[arg(long)]
    memory: bool,
    /// Interface to listen on (use 0.0.0.0 to accept remote connections).
    #[arg(long, env = "NEXA_HOST", default_value = "127.0.0.1")]
    host: String,
    /// Port to listen on.
    #[arg(short, long, env = "NEXA_PORT", default_value_t = 7285)]
    port: u16,
    /// Require `Authorization: Bearer <token>` on every API call.
    #[arg(long, env = "NEXA_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Durability of commits.
    #[arg(long, value_enum, default_value = "always")]
    sync: Sync,
    /// Run an .nql file at startup (e.g. your schema), repeatable.
    #[arg(long, value_name = "FILE")]
    apply: Vec<PathBuf>,
    /// Allow browser requests from this origin, repeatable.
    #[arg(long, value_name = "ORIGIN")]
    cors: Vec<String>,
    /// Page cache size in MiB. Data beyond it stays on disk and is read on
    /// demand, so this bounds memory use, not database size.
    #[arg(long, env = "NEXA_CACHE_MB", default_value_t = 256)]
    cache_mb: usize,
    #[arg(long, hide = true)]
    quiet: bool,
}

impl ServeArgs {
    fn opts(&self) -> server::ServeOpts {
        server::ServeOpts {
            data: self.data.clone().unwrap_or_else(paths::default_data_dir),
            memory: self.memory,
            host: self.host.clone(),
            port: self.port,
            token: self.token.clone(),
            sync: match self.sync {
                Sync::Always => nexadb::SyncMode::Always,
                Sync::Never => nexadb::SyncMode::Never,
            },
            apply: self.apply.clone(),
            cors: self.cors.clone(),
            cache_mb: self.cache_mb,
            quiet: self.quiet,
        }
    }
}

#[derive(Args, Clone)]
struct CtlArgs {
    /// Data directory of the server.
    #[arg(long, env = "NEXA_DATA", value_name = "DIR")]
    data: Option<PathBuf>,
    /// Server address (default: the one recorded by `nexa start`).
    #[arg(long, env = "NEXA_URL")]
    url: Option<String>,
    /// API token.
    #[arg(long, env = "NEXA_TOKEN", hide_env_values = true)]
    token: Option<String>,
}

impl CtlArgs {
    fn data(&self) -> PathBuf {
        self.data.clone().unwrap_or_else(paths::default_data_dir)
    }
}

#[derive(Args, Clone)]
struct ConnArgs {
    /// Server to connect to [default: the running server, or http://127.0.0.1:7285].
    #[arg(long, env = "NEXA_URL")]
    url: Option<String>,
    /// Open a database directory directly instead of connecting to a server.
    #[arg(long, value_name = "DIR", conflicts_with = "url")]
    data: Option<PathBuf>,
    /// Use a throwaway in-memory database.
    #[arg(long, conflicts_with_all = ["url", "data"])]
    memory: bool,
    /// API token.
    #[arg(long, env = "NEXA_TOKEN", hide_env_values = true)]
    token: Option<String>,
}

impl ConnArgs {
    /// Connects, returning the handle and a description of where it points.
    fn connect(&self) -> anyhow::Result<(Nexa, String)> {
        if self.memory {
            return Ok((Nexa::memory(), "in-memory database".into()));
        }
        if let Some(dir) = &self.data {
            let db = Nexa::open(dir).map_err(|e| {
                if e.code() == Some("locked") {
                    anyhow::anyhow!(
                        "{} is being used by a running server — connect to it with `nexa shell` (no --data)",
                        dir.display()
                    )
                } else {
                    anyhow::Error::new(e)
                }
            })?;
            return Ok((db, dir.display().to_string()));
        }
        let (url, _) = daemon::target(&paths::default_data_dir(), self.url.as_deref());
        let db = Nexa::connect_with_token(&url, self.token.as_deref()).map_err(|_| {
            anyhow::anyhow!(
                "can't reach NexaDB at {url}.\n  Start it with `nexa start`, or open a directory directly with `--data DIR`."
            )
        })?;
        Ok((db, url))
    }
}

fn parse_param(raw: &str) -> anyhow::Result<(String, serde_json::Value)> {
    let Some((name, value)) = raw.split_once('=') else {
        bail!("parameters look like name=value, got '{raw}'");
    };
    let value =
        serde_json::from_str(value).unwrap_or_else(|_| serde_json::Value::String(value.to_owned()));
    Ok((name.trim_start_matches('$').to_owned(), value))
}

const EXAMPLE_SCHEMA: &str = include_str!("../assets/example.nql");

fn init_logging(quiet: bool) {
    use std::io::IsTerminal;
    let default = if quiet { "info" } else { "warn" };
    let filter = tracing_subscriber::EnvFilter::try_from_env("NEXA_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
        .init();
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.cmd {
        Cmd::Serve(args) => {
            init_logging(args.quiet);
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(server::serve(args.opts()))?;
        }
        Cmd::Start(args) => daemon::start(&args.opts())?,
        Cmd::Stop(args) => {
            daemon::stop(&args.data(), args.url.as_deref(), args.token.as_deref())?;
        }
        Cmd::Restart(args) => {
            let opts = args.opts();
            daemon::stop(&opts.data, None, opts.token.as_deref())?;
            daemon::start(&opts)?;
        }
        Cmd::Status(args) => {
            if !daemon::status(&args.data(), args.url.as_deref(), args.token.as_deref())? {
                return Ok(ExitCode::from(3));
            }
        }
        Cmd::Logs {
            follow,
            lines,
            data,
        } => {
            daemon::logs(&data.unwrap_or_else(paths::default_data_dir), lines, follow)?;
        }
        Cmd::Shell(conn) => {
            let (db, target) = conn.connect()?;
            shell::Shell::new(db).repl(&target)?;
        }
        Cmd::Query {
            nql,
            params,
            json,
            conn,
        } => {
            let (db, _) = conn.connect()?;
            let mut q = db.query(&nql);
            for p in &params {
                let (k, v) = parse_param(p)?;
                q = q.bind(&k, v);
            }
            let started = Instant::now();
            let rows = q.fetch()?;
            shell::print(&rows, started.elapsed().as_secs_f64() * 1000.0, json, true);
        }
        Cmd::Run { file, conn } => {
            let script = std::fs::read_to_string(&file)
                .with_context(|| format!("cannot read {}", file.display()))?;
            let (db, _) = conn.connect()?;
            let started = Instant::now();
            let rows = db.execute(&script)?;
            println!("{}", brand::ok(&format!("ran {}", file.display())));
            println!(
                "{}",
                table::footer(&rows, started.elapsed().as_secs_f64() * 1000.0)
            );
        }
        Cmd::Schema(conn) => {
            let (db, _) = conn.connect()?;
            let ddl = db.schema()?;
            if ddl.is_empty() {
                println!(
                    "{}",
                    brand::dim("-- no types declared yet (try `nexa init`)")
                );
            } else {
                println!("{ddl}");
            }
        }
        Cmd::Snapshot(conn) => {
            if let Some(dir) = &conn.data {
                nexadb::Database::open(dir)?.snapshot()?;
            } else {
                let (url, _) = daemon::target(&paths::default_data_dir(), conn.url.as_deref());
                if daemon::call(
                    "POST",
                    &format!("{url}/v1/admin/snapshot"),
                    conn.token.as_deref(),
                )?
                .is_none()
                {
                    bail!("can't reach NexaDB at {url}");
                }
            }
            println!(
                "{}",
                brand::ok("checkpoint written; the database file was compacted")
            );
        }
        Cmd::Init { dir } => {
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("schema.nql");
            if path.exists() {
                bail!("{} already exists", path.display());
            }
            std::fs::write(&path, EXAMPLE_SCHEMA)?;
            println!("{}", brand::ok(&format!("wrote {}", path.display())));
            println!("\n  {}", brand::bold("Next:"));
            println!("    nexa start --apply {}", path.display());
            println!("    nexa shell");
            println!("    {}", brand::dim("nexa › SHOW NODES;"));
        }
        Cmd::Service { action } => match action {
            ServiceCmd::Install(args) => daemon::service_install(&args.opts())?,
            ServiceCmd::Uninstall => daemon::service_uninstall()?,
            ServiceCmd::Print(args) => {
                let exe = std::env::current_exe()?;
                println!("# {}", paths::service_file().display());
                print!("{}", daemon::service_definition(&exe, &args.opts()));
            }
        },
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{}", brand::err(&format!("{e:#}")));
            ExitCode::FAILURE
        }
    }
}
