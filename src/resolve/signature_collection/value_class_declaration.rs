//! What a `value class` declaration is on the compilation target, and the shape diagnostics the
//! reference compilers report for it.
//!
//! The JVM makes a single-field `value class` inline only with `@JvmInline`; every other target
//! reads the keyword alone. A multi-field one is a full value class behind `+FullValueClasses` on
//! every target. Positions follow the reference compilers: a missing feature or annotation is
//! reported at the `value` keyword, a wrong parameter count at the primary-constructor parameter
//! list.

use crate::compilation_target::CompilationTarget;
use crate::diag::{DiagSink, Span};

/// The written shape of one `value class` declaration.
pub(super) struct ValueClassDeclaration {
    pub(super) value_keyword: Span,
    pub(super) parameters: Option<Span>,
    pub(super) parameter_count: usize,
    pub(super) jvm_inline: bool,
    pub(super) final_class: bool,
}

/// How the declaration is represented once its shape is checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ValueClassRepresentation {
    /// Unboxed as its single underlying property.
    Inline,
    /// A boxed full value class with structural `equals`/`hashCode`/`toString`.
    Full,
    /// Rejected with a diagnostic; collected as an ordinary class.
    Rejected,
}

impl ValueClassDeclaration {
    pub(super) fn representation(
        &self,
        target: CompilationTarget,
        full_value_classes: bool,
        diags: &mut DiagSink,
    ) -> ValueClassRepresentation {
        let annotation_required = target.inline_value_classes_require_jvm_inline();
        // Only the JVM reads the annotation; elsewhere it is not part of the representation.
        let jvm_inline = annotation_required && self.jvm_inline;
        let Some(parameters) = self.parameters else {
            if !full_value_classes {
                diags.error(
                    self.value_keyword,
                    "primary constructor is required for value classes.",
                );
                return ValueClassRepresentation::Rejected;
            }
            if self.final_class {
                diags.error(
                    self.value_keyword,
                    "primary constructor is required for final value classes.",
                );
                return ValueClassRepresentation::Rejected;
            }
            return ValueClassRepresentation::Full;
        };
        let wrong_parameter_count = if jvm_inline && full_value_classes {
            "@JvmInline value class must have exactly one primary constructor parameter."
        } else {
            "value class must have exactly one primary constructor parameter."
        };
        match self.parameter_count {
            1 if annotation_required && !jvm_inline && !full_value_classes => {
                diags.error(
                    self.value_keyword,
                    "value classes without '@JvmInline' annotation are not yet supported.",
                );
                ValueClassRepresentation::Rejected
            }
            1 if jvm_inline || !full_value_classes => ValueClassRepresentation::Inline,
            1 => ValueClassRepresentation::Full,
            0 if !jvm_inline && full_value_classes => {
                if self.final_class {
                    diags.error(
                        parameters,
                        "final value class must have at least one primary constructor parameter.",
                    );
                    ValueClassRepresentation::Rejected
                } else {
                    ValueClassRepresentation::Full
                }
            }
            0 => {
                diags.error(parameters, wrong_parameter_count);
                ValueClassRepresentation::Rejected
            }
            _ if jvm_inline => {
                diags.error(parameters, wrong_parameter_count);
                ValueClassRepresentation::Rejected
            }
            _ if full_value_classes => ValueClassRepresentation::Full,
            _ => {
                diags.error(
                    self.value_keyword,
                    "the feature \"full value classes\" is experimental and should be enabled \
                     explicitly. This can be done by supplying the compiler argument \
                     '-XXLanguage:+FullValueClasses', but note that no stability guarantees are \
                     provided.",
                );
                ValueClassRepresentation::Rejected
            }
        }
    }
}
