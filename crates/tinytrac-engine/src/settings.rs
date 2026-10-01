use std::fmt;

use jiff::civil::Weekday;
use serde::{Deserialize, Serialize};
use tinytrac_core::BreakPolicy;

/// Preferences shared by all of a user's devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Whether new timers use the break bank.
    pub break_bank: bool,
    /// Kept while the break bank is off, so turning it back on restores it.
    pub break_policy: BreakPolicy,
    pub week_start: WeekStart,
}

impl Settings {
    /// The break policy for new timers, if the break bank is on.
    pub fn breaks(&self) -> Option<BreakPolicy> {
        self.break_bank.then_some(self.break_policy)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeekStart {
    #[default]
    Monday,
    Sunday,
}

impl From<WeekStart> for Weekday {
    fn from(start: WeekStart) -> Self {
        match start {
            WeekStart::Monday => Weekday::Monday,
            WeekStart::Sunday => Weekday::Sunday,
        }
    }
}

impl fmt::Display for WeekStart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            WeekStart::Monday => "Monday",
            WeekStart::Sunday => "Sunday",
        })
    }
}
