//! `external` modifiers written on a target kotlinc deprecates for it.
//!
//! kotlinc's modifier checker accepts `external` on an enum entry and on a primary-constructor
//! property only while `ForbidExternalEnumEntriesAndPrimaryConstructorProperties` is off. The
//! declarations themselves keep no `external` flag, so the parser records each such keyword where
//! it consumes the modifier list.

use crate::diag::Span;

/// The declaration an `external` modifier was written on, as kotlinc's modifier checker names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalModifierTarget {
    EnumEntry,
    /// A primary-constructor `val`/`var` parameter, which kotlinc reports as a value parameter.
    PropertyParameter,
}

impl ExternalModifierTarget {
    /// The target's name in kotlinc's modifier diagnostics.
    pub fn description(self) -> &'static str {
        match self {
            Self::EnumEntry => "enum entry",
            Self::PropertyParameter => "value parameter",
        }
    }
}

/// One `external` keyword on a deprecated target, with the keyword's own span and its position in
/// that declaration's modifier list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExternalModifierUse {
    pub target: ExternalModifierTarget,
    pub span: Span,
    /// Another `external` preceded this one in the same modifier list.
    pub repeated: bool,
}
