//! The meaning of a `@kotlin.Deprecated` application: kotlinc's `DeprecationInfo` for a
//! declaration of any origin.
//!
//! A provider records the level as a semantic fact of the application
//! ([`super::AnnotationSemanticFacts`]) while the selected enum entry is still in hand; the
//! message is the application's `message` element. Consumers ask the application, never its
//! spelling.
//!
//! A dependency declaration states its deprecation before the compilation's API version is known
//! ([`DeclaredDeprecation`]): `@DeprecatedSinceKotlin` derives the level from that version, and a
//! Java declaration is deprecated by its `Deprecated` attribute alone. Its provider resolves the
//! level and publishes it with the declaration's diagnostic rendering
//! ([`DeprecatedDeclaration`]).

use super::{type_name, AnnotationValue, ResolvedAnnotation};
use crate::language_version::LanguageVersion;

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

/// `@kotlin.DeprecatedSinceKotlin`: the API versions from which a deprecation is a warning, an
/// error, and hidden.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeprecatedSinceKotlin {
    pub warning_since: Option<LanguageVersion>,
    pub error_since: Option<LanguageVersion>,
    pub hidden_since: Option<LanguageVersion>,
}

impl DeprecatedSinceKotlin {
    /// The application's versions, when it is `@kotlin.DeprecatedSinceKotlin`.
    pub fn of(annotation: &ResolvedAnnotation) -> Option<Self> {
        if annotation.annotation != type_name("kotlin/DeprecatedSinceKotlin") {
            return None;
        }
        let version = |element: &str| {
            annotation
                .arguments
                .iter()
                .find_map(|(name, value)| match value {
                    AnnotationValue::String(version) if name == element => {
                        LanguageVersion::from_major_minor_text(&version.to_lossy())
                    }
                    _ => None,
                })
        };
        Some(Self {
            warning_since: version("warningSince"),
            error_since: version("errorSince"),
            hidden_since: version("hiddenSince"),
        })
    }

    /// The most severe level `api_version` has reached, if any.
    fn level(self, api_version: LanguageVersion) -> Option<DeprecationLevel> {
        let reached =
            |since: Option<LanguageVersion>| since.is_some_and(|since| since <= api_version);
        if reached(self.hidden_since) {
            Some(DeprecationLevel::Hidden)
        } else if reached(self.error_since) {
            Some(DeprecationLevel::Error)
        } else if reached(self.warning_since) {
            Some(DeprecationLevel::Warning)
        } else {
            None
        }
    }
}

/// The deprecation a dependency declaration states, before the compilation's API version selects
/// its level.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeclaredDeprecation {
    /// `@kotlin.Deprecated`, with the `@kotlin.DeprecatedSinceKotlin` that replaces its level.
    Kotlin {
        deprecation: Deprecation,
        since: Option<DeprecatedSinceKotlin>,
    },
    /// A Java declaration carrying the `Deprecated` attribute or `@java.lang.Deprecated`.
    Java,
}

impl DeclaredDeprecation {
    /// The Kotlin deprecation `annotations` declare.
    pub fn of_annotations(annotations: &[ResolvedAnnotation]) -> Option<Self> {
        let deprecation = annotations
            .iter()
            .find_map(ResolvedAnnotation::deprecation)?;
        Some(Self::Kotlin {
            deprecation,
            since: annotations.iter().find_map(DeprecatedSinceKotlin::of),
        })
    }

    /// The deprecation in effect under `api_version`. `@DeprecatedSinceKotlin` replaces the
    /// declared level, and deprecates nothing before its first version.
    pub fn effective(&self, api_version: LanguageVersion) -> Option<Deprecation> {
        match self {
            Self::Kotlin {
                deprecation,
                since: None,
            } => Some(deprecation.clone()),
            Self::Kotlin {
                deprecation,
                since: Some(since),
            } => since.level(api_version).map(|level| Deprecation {
                level,
                message: deprecation.message.clone(),
            }),
            Self::Java => Some(Deprecation {
                level: DeprecationLevel::Warning,
                message: Some("Deprecated in Java".to_string()),
            }),
        }
    }
}

/// A dependency declaration's deprecation in effect, with the SYMBOL rendering its diagnostic
/// names. Providers publish it on the declaration; a use of the declaration reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeprecatedDeclaration {
    pub deprecation: Deprecation,
    pub symbol: Box<str>,
}
