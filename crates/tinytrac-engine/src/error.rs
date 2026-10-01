use tinytrac_core::{SessionError, TagError, TimerError};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("stored data is unreadable: {0}")]
    Data(#[from] serde_json::Error),
    #[error(transparent)]
    Tag(#[from] TagError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Timer(#[from] TimerError),
    #[error("{0} not found")]
    NotFound(&'static str),
    #[error("that tag already has an open timer")]
    TagBusy,
}
