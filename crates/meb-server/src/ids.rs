//! Server-generated identifiers.
//!
//! Ids are random UUIDv4 values created only by the server. A client can
//! *present* an id (in a URL or JSON field) but never choose one. Parsing is
//! strict: only the canonical lowercase hyphenated v4 form is accepted, so
//! one resource has exactly one textual id and anything else (braces, URN
//! form, uppercase, path fragments, other UUID versions) is simply
//! "not found".

use std::fmt;
use uuid::Uuid;

macro_rules! server_id {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        pub struct $name(Uuid);

        impl $name {
            pub(crate) fn generate() -> Self {
                $name(Uuid::new_v4())
            }

            /// `None` for anything that is not a canonical v4 id.
            pub fn parse(raw: &str) -> Option<Self> {
                let uuid = Uuid::parse_str(raw).ok()?;
                if uuid.get_version_num() != 4 || uuid.hyphenated().to_string() != raw {
                    return None;
                }
                Some($name(uuid))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0.hyphenated())
            }
        }
    };
}

server_id!(FileId);
server_id!(JobId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_round_trip_and_are_unique() {
        let a = FileId::generate();
        let b = FileId::generate();
        assert_ne!(a, b);
        assert_eq!(FileId::parse(&a.to_string()), Some(a));
    }

    #[test]
    fn non_canonical_or_hostile_forms_are_rejected() {
        let id = JobId::generate().to_string();
        assert!(JobId::parse(&id.to_uppercase()).is_none());
        assert!(JobId::parse(&id.replace('-', "")).is_none());
        assert!(JobId::parse(&format!("{{{id}}}")).is_none());
        assert!(JobId::parse(&format!("urn:uuid:{id}")).is_none());
        assert!(JobId::parse("../../etc/passwd").is_none());
        assert!(JobId::parse("").is_none());
        // A valid UUID of another version is not a server-issued id.
        assert!(JobId::parse("00000000-0000-1000-8000-000000000000").is_none());
    }
}
