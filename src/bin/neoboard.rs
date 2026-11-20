use clap::{Parser, Subcommand};

mod update;

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
}

fn main() {
    match Cli::parse().command {
        Command::Update => {
            if let Err(e) = update::run() {
                eprintln!("Update failed: {e}");
                std::process::exit(1);
            }
        }
    }
}
