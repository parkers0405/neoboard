//! Create local boards and share them with private HTTPS invite links.
use clap::{Parser, Subcommand};
use neochat::{client::connect_local, protocol::Action, sharing::share_local};
use reqwest::Url;
use std::{error::Error, path::{Path, PathBuf}, process::Command};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Parser)]
#[command(about = "Create boards and share private Neoboard invite links")]
struct Args {
    #[command(subcommand)]
    command: Mode,
}

#[derive(Subcommand)]
enum Mode {
    /// Create or reuse a board's general channel on the local daemon.
    Create {
        #[arg(long)] data_dir: Option<PathBuf>,
        /// Also publish the board and print a private invite link for this guest.
        #[arg(long, value_name = "ACTOR")] share_with: Option<String>,
        project: String,
    },
    /// Publish an existing board with the bundled SDK and print a private invite link.
    Share {
        #[arg(long)] data_dir: Option<PathBuf>,
        project: String,
        actor: String,
    },
    /// Run a foreground Tailscale Serve session on a dedicated HTTPS port.
    Tailnet {
        #[arg(long)] data_dir: Option<PathBuf>,
        #[arg(long, default_value_t = 8443, value_parser = clap::value_parser!(u16).range(1024..))]
        port: u16,
    },
}

fn print_link(data_dir: Option<&Path>, project: &str, actor: &str) -> Result<()> {
    println!("{}", share_local(data_dir, project, actor)?.to_link()?);
    Ok(())
}

fn run(args: Args) -> Result<()> {
    match args.command {
        Mode::Create { data_dir, project, share_with } => {
            if !neochat::valid_project(&project) {
                return Err("invalid board ID; use 1–80 ASCII letters, digits, dots, dashes or underscores".into());
            }
            let local = connect_local(data_dir.as_deref(), None)?;
            local.action(&Action::CreateChannel { channel: format!("{project}/general") })?;
            if let Some(actor) = share_with {
                print_link(data_dir.as_deref(), &project, &actor)?;
            } else {
                println!("{project}");
            }
        }
        Mode::Share { data_dir, project, actor } => print_link(data_dir.as_deref(), &project, &actor)?,
        Mode::Tailnet { data_dir, port } => {
            let client = connect_local(data_dir.as_deref(), None)?;
            let target = Url::parse(client.endpoint())?;
            if target.scheme() != "http" || !matches!(target.host_str(), Some("127.0.0.1" | "[::1]")) {
                return Err("Tailscale Serve requires a loopback HTTP daemon".into());
            }
            // Refuse to change an existing serve configuration, including a Funnel.
            let status = Command::new("tailscale").args(["serve", "status", "--json"]).output()?;
            if !status.status.success() { return Err("couldn't inspect Tailscale Serve; is Tailscale running?".into()); }
            let config: serde_json::Value = if status.stdout.iter().all(u8::is_ascii_whitespace) {
                serde_json::json!({})
            } else { serde_json::from_slice(&status.stdout)? };
            if !config.is_null() && !config.as_object().is_some_and(|o| o.is_empty()) {
                return Err("Tailscale already has Serve/Funnel configuration; use an existing private proxy rather than replacing it".into());
            }
            let mut command = Command::new("tailscale");
            command.arg("serve").arg(format!("--https={port}")).arg(client.endpoint());
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                return Err(command.exec().into());
            }
            #[cfg(not(unix))]
            {
                if !command.status()?.success() { return Err("Tailscale Serve exited unsuccessfully".into()); }
            }
        }
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run(Args::parse()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => { eprintln!("neochat-share: {err}"); std::process::ExitCode::FAILURE }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_share_requires_an_explicit_board_and_actor() {
        assert!(Args::try_parse_from(["neochat-share", "share", "team"]).is_err());
        assert!(Args::try_parse_from(["neochat-share", "share", "team", "guest"]).is_ok());
        assert!(Args::try_parse_from(["neochat-share", "create", "team", "--share-with", "guest"]).is_ok());
    }

    #[test]
    fn file_invite_commands_are_not_available() {
        use clap::CommandFactory;
        let cli = Args::command();
        for removed in ["invite", "check", "open"] {
            assert!(cli.find_subcommand(removed).is_none());
        }
    }
}