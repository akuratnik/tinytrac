//! The app's screens. Each owns its edit state and messages, calls the engine
//! in `update`, and renders from the shared [`Data`](crate::data::Data).

pub mod history;
pub mod reports;
pub mod settings;
pub mod timers;

use iced::widget::{Text, text};

/// Something to tell the user: a failed command or invalid input.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Engine(#[from] tinytrac_engine::Error),
    #[error("{0}")]
    Input(&'static str),
}

pub type Result<T = Outcome, E = Error> = std::result::Result<T, E>;

/// What a screen's `update` did, so the app knows whether to reload its data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Only the screen's own state changed, such as typing into a field.
    Edited,
    /// The engine saved something.
    Saved,
}

pub fn heading(label: &str) -> Text<'_> {
    text(label).size(18)
}

pub fn muted<'a>(label: impl text::IntoFragment<'a>) -> Text<'a> {
    text(label).size(13).style(text::secondary)
}
