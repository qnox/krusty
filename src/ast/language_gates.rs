//! Language-feature gates one parsed file carries: the uses of syntax its language settings do not
//! support, found while parsing, and the gates of features whose uses only checking can find.

use std::sync::Arc;

use crate::diag::Span;
use crate::features::{DeprecationGate, FeatureGate};

/// One use of syntax whose language feature is disabled, with the error kotlinc reports for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsupportedSyntax {
    pub span: Span,
    pub message: Arc<str>,
}

#[derive(Default)]
pub struct LanguageGates {
    /// Rejected syntax in parse order. The parser retains these rather than reporting them, so a
    /// gated use does not count as a syntax error that keeps the file's declarations out of the
    /// module; the frontend reports them once the file is admitted.
    pub unsupported_syntax: Vec<UnsupportedSyntax>,
    /// `CompanionBlocksAndExtensions`, for references to companion-block members and companion
    /// extensions.
    pub companion_blocks_and_extensions: FeatureGate,
    /// `CollectionLiterals`, for the meaning of a `[…]` expression outside an annotation.
    pub collection_literals: FeatureGate,
    /// `FunctionalTypeWithExtensionAsSupertype`, for supertypes that expand to an extension or
    /// contextual function type.
    pub functional_type_with_extension_as_supertype: FeatureGate,
    /// `ForbidEnumEntryNamedEntries`: the severity of an enum entry named `entries`, which is an
    /// error exactly when the feature is enabled; enabled, such an entry also loses to `Enum.entries`.
    pub enum_entry_named_entries: DeprecationGate,
    /// `ProhibitIntersectionReifiedTypeParameter`, the severity of an intersection type argument
    /// for a reified type parameter.
    pub intersection_reified_type_parameter: DeprecationGate,
    /// `IntrinsicConstEvaluation`: the standard-library operations a constant expression may
    /// apply, beyond the operators and conversions every language version folds.
    pub intrinsic_const_evaluation: bool,
    /// The language features kotlinc's value-class declaration checker consults.
    pub value_classes: ValueClassRules,
}

/// The rules of kotlinc's value-class declaration checker under one file's language settings.
#[derive(Clone, Debug, Default)]
pub struct ValueClassRules {
    pub arity: ValueClassArity,
    /// `CustomEqualsInValueClasses`: a value class may declare `equals` and `hashCode`, and an
    /// `operator fun equals` taking the class itself is its typed equality.
    pub custom_equals: bool,
    /// `AllowExpectValueClassesWithNoPrimaryConstructor`: an `expect` value class may leave its
    /// primary constructor to the `actual`.
    pub expect_without_primary_constructor: bool,
}

/// How many primary-constructor parameters a value class represented inline may declare. The
/// reference releases differ in which feature decides it.
#[derive(Clone, Debug)]
pub enum ValueClassArity {
    /// The releases that have `JvmInlineMultiFieldValueClasses` (2.4.0, 2.4.10): one parameter,
    /// or any positive number when the feature is enabled.
    MultiFieldFeature { enabled: bool },
    /// The releases without it (2.4.20): exactly one. A `value class` that wrote more parameters
    /// and no `@JvmInline` is a use of `FullValueClasses`, reported with this gate.
    Single { full_value_classes: FeatureGate },
}

impl Default for ValueClassArity {
    fn default() -> Self {
        Self::Single {
            full_value_classes: FeatureGate::default(),
        }
    }
}

impl LanguageGates {
    /// Retain a use of `gate`'s syntax at `span` when that feature is disabled.
    pub fn require(&mut self, gate: &FeatureGate, span: Span) {
        if let Some(message) = gate.unsupported_message() {
            self.unsupported_syntax.push(UnsupportedSyntax {
                span,
                message: message.clone(),
            });
        }
    }

    /// Retain a syntax error that does not depend on a feature but is reported with the gates.
    pub fn reject(&mut self, span: Span, message: &str) {
        self.unsupported_syntax.push(UnsupportedSyntax {
            span,
            message: message.into(),
        });
    }
}
