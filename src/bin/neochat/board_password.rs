use super::{print_json, print_lines, Client, Result};
use clap::Subcommand;
use neochat::client::MAX_BOARD_PASSWORD_BYTES;
use std::io::{self, IsTerminal, Read};

#[cfg(unix)]
#[path = "tty_echo.rs"]
mod tty_echo;

#[derive(Subcommand)]
pub(super) enum BoardPassword {
    /// Set or rotate the password. Prompts twice without echo on a terminal.
    Set {
        project: String,
        /// Read one UTF-8 password from a pipe or redirected file, without prompting.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Disable password joins for the project. Does not prompt.
    Disable { project: String },
}

impl BoardPassword {
    pub(super) fn run(self, client: &Client, json: bool) -> Result<()> {
        let enabled = match self {
            Self::Set { project, password_stdin } => {
                if !neochat::valid_project(&project) { return Err("invalid board project".into()); }
                let password = read_password(password_stdin)?;
                client.set_board_password(&project, Some(&password))?;
                true
            }
            Self::Disable { project } => {
                client.set_board_password(&project, None)?;
                false
            }
        };
        if json { print_json(&serde_json::json!({"enabled": enabled})) }
        else { print_lines([if enabled { "Board password set." } else { "Board password disabled." }]) }
    }
}

fn check_input(password: &str) -> Result<()> {
    if password.is_empty() || password.len() > MAX_BOARD_PASSWORD_BYTES || password.contains(['\r', '\n', '\0']) {
        return Err("expected one nonempty password of at most 1024 UTF-8 bytes (no NUL or line breaks)".into());
    }
    Ok(())
}

pub(super) fn read_password(password_stdin: bool) -> Result<String> {
    let stdin = io::stdin();
    match (password_stdin, stdin.is_terminal()) {
        (true, false) => read_pipe(stdin.lock()),
        (true, true) => Err("--password-stdin requires a pipe or redirected file, not a terminal".into()),
        (false, false) => Err("use --password-stdin to set a password noninteractively".into()),
        (false, true) => {
            // rpassword displays the prompt just before configuring the TTY.
            // A fast paste/PTY can otherwise be echoed during that interval.
            #[cfg(unix)]
            let _guard = tty_echo::Guard::new().map_err(|_| "could not disable password echo")?;
            let hidden = |prompt| rpassword::prompt_password(prompt).map_err(|_| "could not read hidden password");
            let password = hidden("New board password: ")?;
            check_input(&password)?;
            if hidden("Confirm board password: ")? != password { return Err("passwords did not match".into()); }
            Ok(password)
        }
    }
}

fn read_pipe(reader: impl Read) -> Result<String> {
    let mut password = String::new();
    // Room for a trailing CRLF and one byte to detect overflow. Read to EOF
    // so an accidental multiline secret is rejected, not silently truncated.
    reader.take(MAX_BOARD_PASSWORD_BYTES as u64 + 3).read_to_string(&mut password)
        .map_err(|_| "could not read UTF-8 password from stdin")?;
    if password.ends_with('\n') {
        password.pop();
        if password.ends_with('\r') { password.pop(); }
    }
    check_input(&password)?;
    Ok(password)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_pipe_preserves_spaces_and_removes_only_one_line_ending() {
        for input in [" 密碼 ", " 密碼 \n", " 密碼 \r\n"] {
            assert_eq!(read_pipe(input.as_bytes()).unwrap(), " 密碼 ");
        }
        for input in [b"".as_slice(), b"\n", b"a\n\n", b"a\r", b"a\0b", b"\xff"] {
            assert!(read_pipe(input).is_err());
        }
        let max = "a".repeat(MAX_BOARD_PASSWORD_BYTES);
        assert!(read_pipe(format!("{max}\r\n").as_bytes()).is_ok());
        assert!(read_pipe(format!("{max}a").as_bytes()).is_err());
    }
}