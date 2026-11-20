use serde_json::json;
use std::{env, path::Path, sync::LazyLock};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Agent {
    ClaudeCode,
    ClaudeDesktop,
    Codex,
    #[default]
    Neoism,
}

impl Agent {
    pub const ALL: [Self; 4] = [Self::ClaudeCode, Self::ClaudeDesktop, Self::Codex, Self::Neoism];

    pub fn name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::ClaudeDesktop => "Claude Desktop",
            Self::Codex => "Codex",
            Self::Neoism => "Neoism",
        }
    }
}

pub struct Guide {
    pub intro: String,
    pub instruction: String,
    pub snippet: String,
    pub question: String,
}

static ADAPTER: LazyLock<String> = LazyLock::new(|| {
    let name = format!("neochat-mcp{}", env::consts::EXE_SUFFIX);
    let Some(exe) = env::current_exe().ok() else { return name };
    let sibling = exe.with_file_name(&name);
    let desktop = Path::new(env!("CARGO_MANIFEST_DIR"));
    if cfg!(debug_assertions) && exe.starts_with(desktop.join("target/debug")) && !sibling.is_file() {
        return desktop.with_file_name("target").join("debug").join(name).display().to_string();
    }
    sibling.display().to_string()
});

impl Guide {
    pub fn new(app: &crate::App) -> Self {
        Self::build(app.setup_agent, &ADAPTER, &app.options, app.remote())
    }

    fn build(agent: Agent, adapter: &str, opts: &crate::connection::Options, remote: bool) -> Self {
        let mut args = Vec::new();
        let joined = remote && opts.invite.is_some();
        if let Some(path) = opts.invite.as_ref().filter(|_| joined) {
            args.push("--invite".to_owned());
            args.push(path.display().to_string());
        } else if !remote && opts.data_dir.is_some() {
            let dir = neochat::client::local_data_dir(opts.data_dir.as_deref()).ok()
                .and_then(|p| std::path::absolute(p).ok());
            args.push("--data-dir".to_owned());
            args.push(dir.as_deref().and_then(Path::to_str).unwrap_or("<absolute board data directory>").to_owned());
        }
        let remote = remote && !joined;
        let intro = if joined { "Adding this entry authorizes this agent on this device to use your saved invite for the selected board, with the same board permissions. No token needs to be copied." } else if remote { "This board is hosted. Give the connection its own board credential." } else { "Open NeoBoard and leave it running. The connection can see the boards available on this local server." }.to_owned();
        let (location, snippet) = match agent {
            Agent::Codex => {
                // JSON string escapes also work in TOML basic strings; DEL must be escaped in TOML.
                let command = json!(adapter).to_string().replace('\u{7f}', "\\u007f");
                let args = json!(args).to_string().replace('\u{7f}', "\\u007f");
                let env = if remote { "\nenv_vars = [\"NEOCHAT_URL\", \"NEOCHAT_TOKEN\"]" } else { "" };
                ("~/.codex/config.toml", format!("[mcp_servers.neoboard]\ncommand = {command}\nargs = {args}{env}"))
            }
            Agent::Neoism => {
                args.insert(0, adapter.to_owned());
                let mut server = json!({"type":"local", "command":args, "enabled":true});
                if remote { server["environment"] = json!({"NEOCHAT_URL":"{env:NEOCHAT_URL}", "NEOCHAT_TOKEN":"{env:NEOCHAT_TOKEN}"}); }
                ("~/.config/neoism/mcp.json", format!("{:#}", json!({"neoboard":server})))
            }
            Agent::ClaudeCode | Agent::ClaudeDesktop => {
                let mut server = json!({"command":adapter, "args":args});
                let location = if agent == Agent::ClaudeCode {
                    if remote { server["env"] = json!({"NEOCHAT_URL":"${NEOCHAT_URL}", "NEOCHAT_TOKEN":"${NEOCHAT_TOKEN}"}); }
                    "the project's .mcp.json"
                } else {
                    if remote { server["env"] = json!({"NEOCHAT_URL":"<board endpoint>", "NEOCHAT_TOKEN":"<agent invite token>"}); }
                    "claude_desktop_config.json (Settings → Developer → Edit Config)"
                };
                (location, format!("{:#}", json!({"mcpServers":{"neoboard":server}})))
            }
        };
        let auth = if joined { "Replace any previous NeoBoard entry. Keep the invite file private and on this device." } else { match (remote, agent) {
            (false, _) => "No workspace selection is needed.",
            (true, Agent::ClaudeDesktop) => "Replace the env placeholders privately in that local file with the board endpoint and this agent's scoped invite token. Do not share the edited file.",
            (true, _) => "Set NEOCHAT_URL to the board endpoint and NEOCHAT_TOKEN to this agent's scoped invite token in the client's launch environment.",
        }};
        Self {
            intro,
            instruction: format!("Merge this entry into {location}. {auth} Restart {}.", agent.name()),
            snippet,
            question: "Using NeoBoard, which channels can you see? List them without making any changes.".into(),
        }
    }

    pub fn copy_text(&self) -> String {
        format!("1. {}\n\n2. {}\n\n{}\n\n3. Ask: {}", self.intro, self.instruction, self.snippet, self.question)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::Options;

    #[test]
    fn selected_clients_get_their_own_config_without_credentials() {
        let opts = Options { token: "PRIVATE_SENTINEL".into(), endpoint: Some("https://PRIVATE_SENTINEL.invalid".into()), ..Options::default() };
        assert_eq!(Agent::default(), Agent::Neoism);
        for (agent, marker) in Agent::ALL.into_iter().zip(["${NEOCHAT_TOKEN}", "<agent invite token>", "env_vars", "{env:NEOCHAT_TOKEN}"]) {
            let guide = Guide::build(agent, "C:\\Neo Board\\adapter.exe", &opts, true);
            let text = guide.copy_text();
            assert!(text.contains(agent.name()) && text.contains(&guide.snippet) && text.contains(&guide.question));
            assert!(guide.snippet.contains(marker));
            assert!(!text.contains("PRIVATE_SENTINEL") && !text.contains("--data-dir"));
        }
    }

    #[test]
    fn joined_board_setup_uses_selected_invite_instead_of_environment() {
        let path = env::temp_dir().join("Board A.neochat");
        let opts = Options { invite: Some(path), token: "PRIVATE_SENTINEL".into(), ..Options::default() };
        for agent in Agent::ALL {
            let guide = Guide::build(agent, "adapter", &opts, true);
            assert!(guide.snippet.contains("--invite") && guide.snippet.contains("Board A.neochat"));
            assert!(!guide.copy_text().contains("PRIVATE_SENTINEL") && !guide.snippet.contains("NEOCHAT_"));
        }
    }

    #[test]
    fn local_board_uses_a_separate_quoted_directory_argument() {
        let standard = Guide::build(Agent::Neoism,"adapter",&Options::default(),false);
        assert!(!standard.copy_text().contains("--data-dir"));
        let dir = env::temp_dir().join("Board A");
        let opts = Options { data_dir: Some(dir.clone()), ..Options::default() };
        let guide = Guide::build(Agent::ClaudeDesktop, "adapter", &opts, false);
        let config: serde_json::Value = serde_json::from_str(&guide.snippet).unwrap();
        assert_eq!(config["mcpServers"]["neoboard"]["args"], json!(["--data-dir", dir]));
        assert!(!guide.snippet.contains("TOKEN"));
    }
}