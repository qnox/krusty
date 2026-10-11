//! The meaning of a `@kotlin.Deprecated` application: kotlinc's `DeprecationInfo` for a
//! declaration of any origin.
//!
//! A provider records the level as a semantic fact of the application
//! ([`super::AnnotationSemanticFacts`]) while the selected enum entry is still in hand; the
//! message is the application's `message` element. Consumers ask the application, never its
//! spelling.

use super::{type_name, AnnotationValue, ResolvedAnnotation};

/// kotlinc's `DeprecationLevelValue`, ordered from the mildest to the most severe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeprecationLevel {
    Warning,
    Error,
    Hidden,
}

/// What a deprecated declaration tells its users.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deprecation {
    pub level: DeprecationLevel,
    pub message: Option<String>,
}

impl ResolvedAnnotation {
    /// The deprecation this application declares, when it is `@kotlin.Deprecated`.
    pub fn deprecation(&self) -> Option<Deprecation> {
        if self.annotation != type_name("kotlin/Deprecated") {
            return None;
        }
        let level = if self.facts.deprecated_hidden {
            DeprecationLevel::Hidden
        } else if self.facts.deprecated_error {
            DeprecationLevel::Error
        } else {
            DeprecationLevel::Warning
        };
        let message =
            self.arguments
                .iter()
                .find_map(|(element, value)| match (element.as_str(), value) {
                    ("message", AnnotationValue::String(message)) => Some(message.to_lossy()),
                    _ => None,
                });
        Some(Deprecation { level, message })
    }
}
