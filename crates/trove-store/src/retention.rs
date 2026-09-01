//! Deleting history the user asked to stop keeping.
//!
//! History is unbounded by default: a clipboard manager that quietly throws
//! things away is worse than one that keeps too much. Retention is therefore
//! opt-in, and when it is on it works in small batches so a database with a
//! million rows never blocks the interface behind one long transaction.

/// How the user asked their history to be kept.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RetentionPolicy {
    /// Nothing is ever deleted by age. History is kept until the user says
    /// otherwise, because quietly throwing things away is the worse failure.
    #[default]
    Unlimited,
    /// Entries older than this many days are deleted.
    Days(u16),
}

/// The narrowest and widest a day limit may be.
///
/// A day is the shortest span that cannot delete something the user copied
/// minutes ago; ten years is long enough that anything beyond it is
/// indistinguishable from keeping everything.
pub const MIN_RETENTION_DAYS: u16 = 1;
pub const MAX_RETENTION_DAYS: u16 = 3_650;

/// Largest number of events one retention pass may delete.
///
/// Deleting is a write, and the writer is shared with capture. A bounded batch
/// keeps a long cleanup from holding the queue while the user is copying.
pub const MAX_RETENTION_BATCH: u32 = 100;

impl RetentionPolicy {
    /// Reads a policy from what the settings hold.
    ///
    /// An out-of-range value falls back to keeping everything rather than to a
    /// clamped one: guessing what a nonsensical setting meant risks deleting
    /// more than the user intended.
    pub fn from_days(days: Option<u16>) -> Self {
        match days {
            Some(days) if (MIN_RETENTION_DAYS..=MAX_RETENTION_DAYS).contains(&days) => {
                Self::Days(days)
            }
            _ => Self::Unlimited,
        }
    }

    /// The instant before which entries may be deleted, or nothing to delete.
    pub fn cutoff_ms(self, now_ms: i64) -> Option<i64> {
        match self {
            Self::Unlimited => None,
            Self::Days(days) => {
                let span = i64::from(days).checked_mul(24 * 60 * 60 * 1_000)?;
                now_ms.checked_sub(span)
            }
        }
    }
}

/// What one retention pass did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetentionOutcome {
    pub deleted_events: u64,
    /// True when the pass hit its batch limit and more remains.
    pub more_remaining: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

    #[test]
    fn history_is_kept_forever_unless_the_user_says_otherwise() {
        assert_eq!(RetentionPolicy::default(), RetentionPolicy::Unlimited);
        assert_eq!(RetentionPolicy::from_days(None), RetentionPolicy::Unlimited);
        assert_eq!(RetentionPolicy::Unlimited.cutoff_ms(1_000), None);
    }

    #[test]
    fn a_day_limit_deletes_everything_older_than_it() {
        let policy = RetentionPolicy::from_days(Some(30));

        assert_eq!(policy, RetentionPolicy::Days(30));
        assert_eq!(policy.cutoff_ms(100 * DAY_MS), Some(70 * DAY_MS));
    }

    #[test]
    fn the_inclusive_boundaries_are_accepted() {
        assert_eq!(
            RetentionPolicy::from_days(Some(MIN_RETENTION_DAYS)),
            RetentionPolicy::Days(MIN_RETENTION_DAYS)
        );
        assert_eq!(
            RetentionPolicy::from_days(Some(MAX_RETENTION_DAYS)),
            RetentionPolicy::Days(MAX_RETENTION_DAYS)
        );
    }

    #[test]
    fn a_nonsensical_limit_keeps_everything_rather_than_guessing() {
        // Clamping 0 to 1 would delete a day of history because a setting was
        // wrong, which is the one direction this must never fail in.
        assert_eq!(
            RetentionPolicy::from_days(Some(0)),
            RetentionPolicy::Unlimited
        );
        assert_eq!(
            RetentionPolicy::from_days(Some(MAX_RETENTION_DAYS + 1)),
            RetentionPolicy::Unlimited
        );
    }

    #[test]
    fn an_impossible_span_deletes_nothing() {
        // Arithmetic that cannot be represented must not collapse into "delete
        // everything before the epoch".
        assert_eq!(
            RetentionPolicy::Days(MAX_RETENTION_DAYS).cutoff_ms(i64::MIN),
            None
        );
    }
}
