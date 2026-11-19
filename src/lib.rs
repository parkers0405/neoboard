pub mod board;
pub mod client;
pub mod protocol;

pub mod invite_file;
pub mod sharing;

pub fn valid_project(id: &str) -> bool {
    !id.is_empty() && id.len() <= 80 && !matches!(id, "." | "..")
        && id.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}

pub use board::{router, Board, BoardError};

pub fn default_data_dir() -> std::path::PathBuf {
    dirs::data_local_dir().unwrap_or_else(|| std::path::PathBuf::from(".")).join("neochat")
}