use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Declares a UUID-backed identifier type. Version 7 UUIDs keep ids roughly time-ordered,
/// which keeps database indexes and sync logs in creation order.
macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Generates a fresh, time-ordered id.
            pub fn generate() -> Self {
                Self(Uuid::now_v7())
            }

            /// The underlying UUID, for storage.
            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                Self(uuid)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

id_type!(
    /// Identifies a [`Tag`](crate::Tag).
    TagId
);
id_type!(
    /// Identifies a [`Session`](crate::Session).
    SessionId
);
