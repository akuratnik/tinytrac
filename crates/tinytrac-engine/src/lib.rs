//! Storage and commands for tinytrac.
//!
//! [`Engine`] wraps a local SQLite database and exposes everything the UI does
//! as commands (start a timer, edit an entry) and queries (totals for a range).
//! The rules themselves live in [`tinytrac_core`]; this crate loads records,
//! applies those rules, and saves the result, one transaction per command.

mod engine;
mod error;
mod settings;
mod store;

pub use engine::{Engine, SessionEdit};
pub use error::{Error, Result};
pub use settings::{Settings, WeekStart};

mod sync;
mod transport;
pub use sync::{SyncIssue, SyncReport};
