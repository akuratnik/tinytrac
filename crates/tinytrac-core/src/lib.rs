//! Domain model for tinytrac.
//!
//! This crate is pure: no storage, no UI, no clock. Every operation that depends
//! on the current time takes it as a parameter, which keeps the rules easy to test.
//!
//! - [`Tag`] is what time is tracked against (a client, a project, a category).
//! - [`Session`] is one timer's run for a tag: work and break segments, optional
//!   break-bank policy, and a manual adjustment.
//! - [`Timers`] holds the open sessions and enforces "one timer per tag" and
//!   "starting a timer pauses the others" (unless started alongside).
//! - [`Totals`] splits logged time into calendar days for reports.

mod break_policy;
mod id;
mod interval;
mod report;
mod session;
mod tag;
mod timers;

pub use break_policy::BreakPolicy;
pub use id::{SessionId, TagId};
pub use interval::Interval;
pub use report::{DateRange, Totals};
pub use session::{Activity, Segment, Session, SessionError, Status, Tally};
pub use tag::{ExternalRef, Tag, TagError};
pub use timers::{StartMode, TimerError, Timers};

pub use jiff::{SignedDuration, Timestamp, civil::Date, tz::TimeZone};
