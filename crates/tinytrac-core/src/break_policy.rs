use std::num::NonZeroU32;

use jiff::SignedDuration;
use serde::{Deserialize, Serialize};

/// How much break time work earns: `break_minutes` of break per `work_minutes` of work.
///
/// Credit accrues continuously, so 10 minutes of work under the default policy
/// earns 2 minutes of break. Zero `break_minutes` is allowed and earns nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BreakPolicy {
    pub break_minutes: u32,
    pub work_minutes: NonZeroU32,
}

impl BreakPolicy {
    /// Break credit earned by `work`.
    pub fn earned(&self, work: SignedDuration) -> SignedDuration {
        let nanos =
            work.as_nanos() * i128::from(self.break_minutes) / i128::from(self.work_minutes.get());
        SignedDuration::from_nanos_i128(nanos)
    }
}

impl Default for BreakPolicy {
    /// Five minutes of break per 25 minutes of work.
    fn default() -> Self {
        Self {
            break_minutes: 5,
            work_minutes: NonZeroU32::new(25).expect("25 is non-zero"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_earns_one_fifth_of_work() {
        let policy = BreakPolicy::default();
        assert_eq!(
            policy.earned(SignedDuration::from_mins(25)),
            SignedDuration::from_mins(5)
        );
        assert_eq!(
            policy.earned(SignedDuration::from_mins(10)),
            SignedDuration::from_mins(2)
        );
    }

    #[test]
    fn zero_work_minutes_is_rejected_when_deserializing() {
        let json = r#"{"break_minutes":5,"work_minutes":0}"#;
        assert!(serde_json::from_str::<BreakPolicy>(json).is_err());
    }
}
