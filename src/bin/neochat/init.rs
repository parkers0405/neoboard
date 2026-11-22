use super::{board_password::read_password, print_json, print_lines, Client, Result};
use std::{io::{self, IsTerminal, Write}, path::Path};

#[derive(clap::Args)]
pub(super) struct Init {
    /// Board name, used verbatim as the board ID (not a display title).
    #[arg(long, value_name = "ID")]
    board: Option<String>,
    /// Existing public HTTPS base URL, without credentials, query or fragment.
    #[arg(long, env = "NEOBOARD_PUBLIC_URL", hide_env_values = true, value_name = "HTTPS")]
    public_url: Option<String>,
    /// Read one UTF-8 password from a pipe or redirected file instead of prompting twice.
    #[arg(long)]
    password_stdin: bool,
}

fn value_or_prompt(value: Option<String>, flag: &str, prompt: &str) -> Result<String> {
    if let Some(value) = value { return Ok(value); }
    if !io::stdin().is_terminal() { return Err(format!("supply {flag} when not using a terminal").into()); }
    eprint!("{prompt}: ");
    io::stderr().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    Ok(value.trim().to_owned())
}

impl Init {
    pub(super) fn run(self, dir: Option<&Path>, json: bool) -> Result<()> {
        let board = value_or_prompt(self.board, "--board", "Board name (ID: letters, digits, dashes, underscores or dots)")?;
        if !neochat::valid_project(&board) { return Err("invalid board ID; use 1–80 letters, digits, dashes, underscores or dots (not . or ..)".into()); }
        let url = value_or_prompt(self.public_url, "--public-url or NEOBOARD_PUBLIC_URL", "Public HTTPS base URL (configure Cloudflare or your reverse proxy separately)")?;
        let base = neochat::invite_file::https_endpoint(&url)?;
        let admin = Client::discover_admin(dir)?;
        let password = read_password(self.password_stdin)?;
        admin.init_board(&board, &password)?;

        let address = format!("{base}#neoboard={board}");
        let note = "Configure Cloudflare or your HTTPS reverse proxy separately. Send the board password privately.";
        if json { print_json(&serde_json::json!({"board": board, "join_address": address, "note": note})) }
        else { print_lines([format!("Desktop join address: {address}"), note.into()]) }
    }
}