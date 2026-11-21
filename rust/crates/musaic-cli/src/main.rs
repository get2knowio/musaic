//! `musaic-cli`: the stdio MCP server for assistants, tool calls into the
//! running Musaic app, `doctor`, and host setup. Every command but `mcp`
//! prints one JSON document on stdout; `mcp` prints protocol messages only.
//! Diagnostics go to stderr. No flag takes a secret.

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use musaic_cli::catalog_cmd;
use musaic_cli::client::Client;
use musaic_cli::commands::{self, EXIT_USAGE, Outcome};
use serde_json::Value;

#[derive(Parser)]
#[command(name = "musaic-cli", version, about = "Musaic's agent CLI")]
struct Cli {
    /// The config directory (default: MUSAIC_CONFIG_DIR, then ~/.musaic).
    #[arg(long, global = true)]
    config_dir: Option<PathBuf>,
    /// The pairing to use (default: the only one with a credential).
    #[arg(long, global = true)]
    pairing: Option<String>,
    /// Read the pairing credential from the scratch-dir file a development
    /// run writes. Only accepted for a config dir under the temp dir or a
    /// directory named `work`.
    #[arg(long, global = true)]
    dev_credentials: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Args, Default)]
struct InputArgs {
    /// The tool input as a JSON object.
    #[arg(long, conflicts_with = "input_file")]
    input: Option<String>,
    /// Read the tool input (a JSON object) from this file.
    #[arg(long)]
    input_file: Option<PathBuf>,
}

#[derive(Args)]
struct SetupArgs {
    /// Print what would change and change nothing.
    #[arg(long)]
    dry_run: bool,
    /// claude-desktop, claude-code or all.
    #[arg(long, default_value = "all")]
    host: String,
}

#[derive(Subcommand)]
enum Command {
    /// Serve MCP on stdin and stdout.
    Mcp,
    /// Print the tool catalog.
    Tools,
    /// Call a tool by its CLI name.
    Call {
        name: String,
        #[command(flatten)]
        input: InputArgs,
    },
    /// Report whether the app, the pairing and the host setup work.
    Doctor,
    /// Add Musaic to Claude Desktop and Claude Code.
    Setup(SetupArgs),
    /// Remove the entries `setup` wrote.
    Uninstall(SetupArgs),
    /// Check or write the generated tool catalog.
    Catalog {
        #[arg(long, conflicts_with = "write")]
        check: bool,
        #[arg(long)]
        write: bool,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// A tool's CLI name (`library-list`, `album-get`, ...) with `--input` or
    /// `--input-file`.
    #[command(external_subcommand)]
    External(Vec<String>),
}

fn read_input(args: &InputArgs) -> Result<Option<Value>, String> {
    let text = if let Some(t) = &args.input {
        t.clone()
    } else if let Some(p) = &args.input_file {
        std::fs::read_to_string(p)
            .map_err(|e| format!("cannot read the input file: {}", e.kind()))?
    } else {
        use std::io::IsTerminal;
        if std::io::stdin().is_terminal() {
            return Ok(None);
        }
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| e.to_string())?;
        s
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|_| "the input is not valid JSON".to_string())
}

fn parse_external(args: &[String]) -> Result<(String, InputArgs), String> {
    let name = args.first().cloned().ok_or("a command is required")?;
    let mut input = InputArgs::default();
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--input" => input.input = Some(it.next().ok_or("--input needs a value")?.clone()),
            "--input-file" => {
                input.input_file = Some(PathBuf::from(
                    it.next().ok_or("--input-file needs a value")?,
                ))
            }
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    if input.input.is_some() && input.input_file.is_some() {
        return Err("--input and --input-file cannot be used together".into());
    }
    Ok((name, input))
}

fn emit(o: Outcome) -> ! {
    println!("{}", o.json);
    std::process::exit(o.code);
}

fn build_client(cli: &Cli) -> Result<(Client, PathBuf), Outcome> {
    let Some(dir) = commands::resolve_config_dir(cli.config_dir.as_deref()) else {
        return Err(commands::usage(
            "no config directory: pass --config-dir or set MUSAIC_CONFIG_DIR",
        ));
    };
    let creds = commands::credential_source(&dir, cli.dev_credentials)
        .map_err(|e| commands::failure(&e))?;
    Ok((Client::new(&dir, creds, cli.pairing.clone()), dir))
}

fn main() {
    let cli = Cli::parse();
    match &cli.command {
        Command::Tools => emit(commands::tools()),
        Command::Catalog { check, write, dir } => {
            let _ = check;
            let mode = if *write {
                catalog_cmd::Mode::Write
            } else {
                catalog_cmd::Mode::Check
            };
            let Some(dir) = dir.clone().or_else(catalog_cmd::default_docs_dir) else {
                eprintln!("cannot find the docs directory; pass --dir");
                std::process::exit(EXIT_USAGE);
            };
            match catalog_cmd::run(mode, &dir) {
                Ok(r) if r.is_clean() => {}
                Ok(r) => {
                    for f in &r.differing {
                        eprintln!("{}: differs from the code", dir.join(f).display());
                    }
                    if mode == catalog_cmd::Mode::Check {
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        Command::Doctor => {
            let (client, dir) = build_client(&cli).unwrap_or_else(|o| emit(o));
            emit(commands::doctor(&client, &dir));
        }
        Command::Setup(a) | Command::Uninstall(a) => {
            let uninstall = matches!(cli.command, Command::Uninstall(_));
            let mut opts =
                musaic_cli::setup::Options::new(musaic_cli::setup::HostPaths::from_env());
            opts.dry_run = a.dry_run;
            opts.host = a.host.clone();
            opts.uninstall = uninstall;
            // Only a config dir given explicitly goes into the entry.
            opts.config_dir = cli.config_dir.clone();
            opts.executable = std::env::current_exe().ok();
            emit(musaic_cli::setup::run(&opts));
        }
        Command::Call { name, input } => {
            let input = read_input(input).unwrap_or_else(|m| emit(commands::usage(&m)));
            let (client, _) = build_client(&cli).unwrap_or_else(|o| emit(o));
            emit(commands::call(&client, name, input));
        }
        Command::External(args) => {
            let (name, input) = parse_external(args).unwrap_or_else(|m| emit(commands::usage(&m)));
            let input = read_input(&input).unwrap_or_else(|m| emit(commands::usage(&m)));
            let (client, _) = build_client(&cli).unwrap_or_else(|o| emit(o));
            emit(commands::call(&client, &name, input));
        }
        Command::Mcp => {
            // stdout is the protocol; everything else goes to stderr.
            let (client, _) = match build_client(&cli) {
                Ok(c) => c,
                Err(o) => {
                    eprintln!("{}", o.json);
                    std::process::exit(o.code);
                }
            };
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap_or_else(|e| {
                    eprintln!("cannot start: {e}");
                    std::process::exit(1)
                });
            let backend: Arc<dyn musaic_mcp::Backend> = Arc::new(client);
            let r = rt.block_on(musaic_mcp::serve_stdio(backend, env!("CARGO_PKG_VERSION")));
            if let Err(e) = r {
                eprintln!("mcp: {e}");
                std::process::exit(1);
            }
        }
    }
}
