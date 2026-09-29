use std::path::Path;

/// Errors of the index library.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io { path: String, source: std::io::Error },
    #[error("formato: {0}")]
    Format(String),
    #[error("no encontrado: {0}")]
    NotFound(String),
    #[error("índice: {0}")]
    Index(String),
}

impl Error {
    pub fn io(path: &Path, source: std::io::Error) -> Self {
        Error::Io { path: path.display().to_string(), source }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
