//! Typed identifiers. Every identifier is local to one hub (Plan.md §2.4, §11.3).

use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! row_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub i64);

        impl $name {
            /// The raw row id as stored in the hub database.
            #[must_use]
            pub const fn get(self) -> i64 {
                self.0
            }
        }

        impl From<i64> for $name {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

row_id!(
    /// A project inside this hub's space.
    ProjectId
);
row_id!(
    /// A repository registered to exactly one project.
    RepositoryId
);
row_id!(
    /// An agent definition.
    AgentId
);
row_id!(
    /// The binding of one agent to one project.
    BindingId
);
row_id!(
    /// A standing rule created from a decision.
    StandingRuleId
);
row_id!(
    /// One slice of the inbox with at most one active run.
    LaneId
);
row_id!(
    /// An inbox event.
    EventId
);
row_id!(
    /// A session: the transcript a run works inside.
    SessionId
);
row_id!(
    /// One run: a stretch of work in one session.
    RunId
);
row_id!(
    /// A machine membership in this space.
    MachineId
);
row_id!(
    /// A model account label tracked by this space.
    AccountId
);
row_id!(
    /// A follow-up or routine.
    TimerId
);
row_id!(
    /// An outbox message.
    OutboxId
);
row_id!(
    /// A memory file.
    MemoryFileId
);
row_id!(
    /// A checkpoint bundle record.
    CheckpointId
);

macro_rules! display_id {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub i64);

        impl $name {
            /// The raw row id as stored in the hub database.
            #[must_use]
            pub const fn get(self) -> i64 {
                self.0
            }

            /// Reads the short form, in either case (`T-0142`, `t-142`), or
            /// the bare number.
            #[must_use]
            pub fn parse(text: &str) -> Option<Self> {
                let text = text.trim();
                let prefix = concat!($prefix, "-");
                let digits = match text.get(..prefix.len()) {
                    Some(head) if head.eq_ignore_ascii_case(prefix) => &text[prefix.len()..],
                    _ => text,
                };
                digits.parse().ok().filter(|id| *id > 0).map(Self)
            }
        }

        impl From<i64> for $name {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($prefix, "-{:04}"), self.0)
            }
        }
    };
}

display_id!(
    /// A task, shown as `T-0142`.
    TaskId,
    "T"
);
display_id!(
    /// A decision, shown as `D-0012`.
    DecisionId,
    "D"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_readable_ids_are_zero_padded() {
        assert_eq!(TaskId(142).to_string(), "T-0142");
        assert_eq!(DecisionId(12).to_string(), "D-0012");
        assert_eq!(TaskId(12345).to_string(), "T-12345");
        for good in ["D-0012", "d-12", " 12 "] {
            assert_eq!(DecisionId::parse(good), Some(DecisionId(12)), "{good}");
        }
        for bad in ["D-", "T-0012", "0", "-3", "D-x"] {
            assert_eq!(DecisionId::parse(bad), None, "{bad}");
        }
    }
}
