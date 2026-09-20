mod agent;
mod candidates;
mod desktopctl;
mod jev;
mod model;

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Args, Parser, Subcommand};

use agent::{Agent, AgentConfig, RunResult};

#[derive(Debug, Parser)]
#[command(
    name = "desktopagent",
    version,
    about = "Jev-driven DesktopCtl desktop agent"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Observe, choose, and execute until done or blocked.
    Run(RunArgs),
    /// Perform exactly one observe/choose/execute cycle.
    Step(RunArgs),
    /// Observe and choose without executing an action.
    Inspect(RunArgs),
}

#[derive(Debug, Clone, Args)]
struct RunArgs {
    /// Goal supplied by the user.
    goal: String,
    #[arg(long)]
    active_window: Option<String>,
    #[arg(long)]
    context: Option<PathBuf>,
    #[arg(long)]
    workspace: Option<PathBuf>,
    #[arg(long)]
    json: bool,
    #[arg(long, default_value_t = 15)]
    max_steps: u32,
    #[arg(long, default_value_t = 0.40)]
    confidence_threshold: f64,
    #[arg(long, default_value_t = 10)]
    desktopctl_timeout: u64,
    #[arg(long, default_value_t = 10)]
    jev_timeout: u64,
    #[arg(long, default_value_t = 60)]
    run_timeout: u64,
    #[arg(long)]
    trace: bool,
    #[arg(long)]
    trace_file: Option<PathBuf>,
}

fn main() {
    load_dotenv();
    let cli = Cli::parse();
    let (command, args) = match cli.command {
        Command::Run(args) => ("run", args),
        Command::Step(args) => ("step", args),
        Command::Inspect(args) => ("inspect", args),
    };
    let json = args.json;
    let config = AgentConfig {
        active_window: args.active_window,
        context_path: args.context,
        workspace: args.workspace,
        max_steps: args.max_steps,
        confidence_threshold: args.confidence_threshold,
        desktopctl_timeout: Duration::from_secs(args.desktopctl_timeout),
        jev_timeout: Duration::from_secs(args.jev_timeout),
        run_timeout: Duration::from_secs(args.run_timeout),
        trace: args.trace,
        trace_file: args.trace_file,
    };
    let result = match Agent::new(config) {
        Ok(agent) => match command {
            "run" => agent.run(&args.goal),
            "step" => agent.step(&args.goal),
            _ => agent.inspect(&args.goal),
        },
        Err(error) => Err(error),
    };
    match result {
        Ok(result) => print_result(result, json),
        Err(error) => {
            let result = RunResult::error(error.to_string());
            print_result(result, json);
            std::process::exit(1);
        }
    }
}

fn load_dotenv() {
    // Loading dotenv is intentionally best-effort. First follow dotenv's normal
    // current-directory search, then support running the development binary by
    // absolute path from outside the repository. The key is never printed or
    // included in traces; installed launcher environments should provide it
    // directly.
    let _ = dotenvy::dotenv();
    if std::env::var_os("TYPESAFE_API_KEY").is_some() {
        return;
    }
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    for directory in executable.ancestors().skip(1) {
        let path = directory.join(".env");
        if path.is_file() {
            let _ = dotenvy::from_path(path);
            if std::env::var_os("TYPESAFE_API_KEY").is_some() {
                return;
            }
        }
    }
    // Cargo builds keep the source manifest location available even when the
    // executable is copied, wrapped, or launched through a path that macOS
    // resolves differently.
    for directory in Path::new(env!("CARGO_MANIFEST_DIR")).ancestors() {
        let path = directory.join(".env");
        if path.is_file() {
            let _ = dotenvy::from_path(path);
            if std::env::var_os("TYPESAFE_API_KEY").is_some() {
                return;
            }
        }
    }
}

fn print_result(result: RunResult, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string(&result).unwrap_or_else(|_| "{}".into())
        );
        return;
    }
    println!("{}", result.message);
    if result.steps > 0 {
        eprintln!("{} in {} ms", result.status, result.elapsed_ms);
    }
}
