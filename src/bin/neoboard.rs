use clap::{Parser, Subcommand};

mod update;
mod wake;

#[derive(Parser)]
#[command(version, about = "Manage a Neoboard release bundle")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download and verify the latest whole bundle (stop board processes first).
    Update,
    /// Start an isolated portable desktop and its companion (not an existing service).
    Desktop(update::portable::Options),
    /// Pair a dedicated agent conversation and control automatic inbox wakes.
    Wake(wake::Options),
}

fn main() {
    let result = match Cli::parse().command {
        Command::Update => update::run(),
        Command::Desktop(options) => update::portable::run(options),
        Command::Wake(options) => wake::run(options).map_err(Into::into),
    };
    if let Err(e) = result {
        eprintln!("Neoboard: {e}");
        std::process::exit(1);
    }
}
