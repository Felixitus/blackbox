use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("CSV error: {0}")]
    Csv(#[from] csv::Error),
    #[error("invalid configuration: {0}")]
    Config(String),
    #[error("collector `{collector}` failed: {message}")]
    Collector {
        collector: &'static str,
        message: String,
    },
    #[error("frame capture: {0}")]
    Capture(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
