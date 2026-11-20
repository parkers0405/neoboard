use base64::{Engine, engine::general_purpose::STANDARD};
use rune::{El, Pixels, el};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Framework { Claude, Codex, OpenCode, Neoism }

impl Framework {
    fn from_sender(sender: Option<&str>) -> Option<Self> {
        let source=sender?;
        let name = source.strip_prefix("mcp:").or_else(||source.strip_prefix("cli:"))?.trim();
        match name.to_ascii_lowercase().as_str() {
            "claude" | "claude code" | "claude-code" | "claude-agent" => Some(Self::Claude),
            "codex" | "openai-codex" | "codex_cli_rs" => Some(Self::Codex),
            "opencode" | "open-code" => Some(Self::OpenCode),
            "neoism" | "neoism-agent" | "neoism agent" => Some(Self::Neoism),
            _ => None,
        }
    }

    fn pixels(self) -> &'static Pixels {
        macro_rules! logo {
            ($name:literal) => {{
                static PIXELS: OnceLock<Pixels> = OnceLock::new();
                PIXELS.get_or_init(|| {
                    let bytes = STANDARD.decode(include_str!(concat!("../assets/frameworks/", $name, ".png.b64")).trim()).expect("embedded framework artwork");
                    let rgba = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).expect("embedded framework PNG").into_rgba8();
                    let pixels = Pixels::new();
                    pixels.set(rgba.width(), rgba.height(), rgba.into_raw());
                    pixels
                })
            }};
        }
        match self {
            Self::Claude => logo!("claude"), Self::Codex => logo!("codex"),
            Self::OpenCode => logo!("opencode"), Self::Neoism => logo!("neoism"),
        }
    }
}

pub fn mark(sender: Option<&str>) -> Option<El> {
    Some(el().size(14., 14.).pixels(Framework::from_sender(sender)?.pixels()))
}

#[test]
fn only_explicit_matching_frameworks_have_artwork() {
    use Framework::*;
    for (tag, expected) in [("mcp:Claude Code", Claude), ("mcp:Codex", Codex), ("mcp:OpenCode", OpenCode), ("mcp:Neoism", Neoism)] {
        assert_eq!(Framework::from_sender(Some(tag)), Some(expected));
        assert!(mark(Some(tag)).is_some());
    }
    for sender in [None, Some("desktop"), Some("mcp"), Some("mcp:Codex-like"), Some("Neoism")] {
        assert!(mark(sender).is_none());
    }
}
