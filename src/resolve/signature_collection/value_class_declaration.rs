//! What a `value class` declaration is on the compilation target, and the shape diagnostics the
//! reference compilers report for it.
//!
//! The JVM makes a single-field `value class` inline only with `@JvmInline`; every other target
//! reads the keyword alone. Positions follow the reference compilers: a missing feature or
//! annotation is reported at the `value` keyword, a wrong parameter count at the primary
//! constructor.
//!
//! How many primary-constructor parameters a value class may declare is a rule of kotlinc's
//! common value-class declaration checker, and the language settings decide it (see
//! [`ValueClassArity`]): `JvmInlineMultiFieldValueClasses` on 2.4.0 and 2.4.10, `FullValueClasses`
//! on 2.4.20, where a multi-field or parameterless class is a full value class. That check is
//! [`ValueClassDeclaration::constructor_check`]; signature collection reports its diagnostic once,
//! and the body checker (`resolve::value_class_checks`) consults the same verdict to decide
//! whether kotlinc goes on to check the parameters.

use crate::ast::{ValueClassArity, ValueClassRules};
use crate::compilation_target::CompilationTarget;
use crate::diag::{DiagSink, Span};
use crate::diagnostic_wording::{
    value_class_empty_constructor, value_class_without_primary_constructor,
    value_class_wrong_parameter_count,
};

/// The written shape of one `value class` declaration.
pub(in crate::resolve) struct ValueClassDeclaration {
    pub(in crate::resolve) value_keyword: Span,
    /// The written primary constructor, from its modifiers or `constructor` keyword through `)`.
    pub(in crate::resolve) constructor: Option<Span>,
    pub(in crate::resolve) parameter_count: usize,
    /// The resolved `kotlin.jvm.JvmInline` is applied.
    pub(in crate::resolve) jvm_inline: bool,
    pub(in crate::resolve) final_class: bool,
    /// A top-level `expect` class: header validation (`frontend::expect_value_classes`) reports
    /// its missing primary constructor, and it never needs `@JvmInline`.
    pub(in crate::resolve) expect: bool,
}

/// How the declaration is represented once its shape is checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ValueClassRepresentation {
    /// Unboxed as its single underlying property.
    Inline,
    /// A boxed full value class with structural `equals`/`hashCode`/`toString`.
    Full,
    /// A `@JvmInline` class of several fields under `JvmInlineMultiFieldValueClasses`. No backend
    /// flattens one yet; it is collected as an ordinary class.
    MultiField,
    /// Rejected with a diagnostic; collected as an ordinary class.
    Rejected,
}

/// kotlinc's verdict on a value class's primary constructor.
pub(in crate::resolve) enum ConstructorCheck {
    Passed,
    /// Checking the class ends here. The diagnostic is absent for an `expect` class, whose header
    /// validation reports it.
    Failed(Option<(Span, String)>),
}

impl ValueClassDeclaration {
    /// The primary-constructor rules of kotlinc's value-class declaration checker under `rules`.
    /// `full_value_classes` is whether `FullValueClasses` is enabled. The checker is common to
    /// every target, and reads `@JvmInline` wherever it resolves.
    pub(in crate::resolve) fn constructor_check(
        &self,
        rules: &ValueClassRules,
        full_value_classes: bool,
    ) -> ConstructorCheck {
        let full = !self.jvm_inline && full_value_classes;
        // kotlinc's `valueModifierPrefix` and `finalOrInlineClassPrefix`.
        let value_prefix = if full_value_classes {
            "@JvmInline value"
        } else {
            "value"
        };
        let final_or_inline_prefix = if !full_value_classes {
            "value"
        } else if full {
            "final value"
        } else {
            "@JvmInline value"
        };
        let absent = || {
            ConstructorCheck::Failed((!self.expect).then(|| {
                (
                    self.value_keyword,
                    value_class_without_primary_constructor(final_or_inline_prefix),
                )
            }))
        };
        let count = self.parameter_count;
        // A full value class may declare any number of parameters, and a non-final one none at
        // all, under every release's arity rule.
        if full {
            return match self.constructor {
                None if self.final_class => absent(),
                Some(constructor) if count == 0 && self.final_class => {
                    ConstructorCheck::Failed(Some((
                        constructor,
                        value_class_empty_constructor(final_or_inline_prefix),
                    )))
                }
                _ => ConstructorCheck::Passed,
            };
        }
        let Some(constructor) = self.constructor else {
            return absent();
        };
        match &rules.arity {
            ValueClassArity::MultiFieldFeature { enabled } => {
                if *enabled {
                    if count == 0 {
                        return ConstructorCheck::Failed(Some((
                            constructor,
                            value_class_empty_constructor("value"),
                        )));
                    }
                } else if count != 1 {
                    return ConstructorCheck::Failed(Some((
                        constructor,
                        value_class_wrong_parameter_count("inline"),
                    )));
                }
                ConstructorCheck::Passed
            }
            ValueClassArity::Single {
                full_value_classes: gate,
            } => {
                if count > 1 && !self.jvm_inline {
                    let message = gate.unsupported_message().expect(
                        "a value class without @JvmInline is full when FullValueClasses is enabled",
                    );
                    return ConstructorCheck::Failed(Some((
                        self.value_keyword,
                        message.to_string(),
                    )));
                }
                if count != 1 {
                    return ConstructorCheck::Failed(Some((
                        constructor,
                        value_class_wrong_parameter_count(value_prefix),
                    )));
                }
                ConstructorCheck::Passed
            }
        }
    }

    pub(super) fn representation(
        &self,
        target: CompilationTarget,
        full_value_classes: bool,
        rules: &ValueClassRules,
        diags: &mut DiagSink,
    ) -> ValueClassRepresentation {
        let annotation_required = target.inline_value_classes_require_jvm_inline();
        let constructor = self.constructor_check(rules, full_value_classes);
        if let ConstructorCheck::Failed(Some((span, message))) = &constructor {
            diags.error(*span, message.as_str());
        }
        // kotlinc's `@JvmInline` applicability check, a JVM checker that runs after the
        // declaration checker: the releases with `JvmInlineMultiFieldValueClasses` report every
        // arity, 2.4.20 a single parameter only, as the annotation would not help another arity
        // there.
        let missing_jvm_inline =
            annotation_required && !self.jvm_inline && !full_value_classes && !self.expect;
        let reported_arity = match rules.arity {
            ValueClassArity::MultiFieldFeature { .. } => true,
            ValueClassArity::Single { .. } => {
                self.constructor.is_some() && self.parameter_count == 1
            }
        };
        if missing_jvm_inline && reported_arity {
            diags.error(
                self.value_keyword,
                "value classes without '@JvmInline' annotation are not yet supported.",
            );
        }
        if matches!(constructor, ConstructorCheck::Failed(_)) {
            return ValueClassRepresentation::Rejected;
        }
        // Only the JVM reads the annotation; elsewhere it is not part of the representation.
        let jvm_inline = annotation_required && self.jvm_inline;
        match self.parameter_count {
            1 if annotation_required && !jvm_inline && !full_value_classes => {
                ValueClassRepresentation::Rejected
            }
            1 if jvm_inline || !full_value_classes => ValueClassRepresentation::Inline,
            _ if !jvm_inline && full_value_classes => ValueClassRepresentation::Full,
            _ => ValueClassRepresentation::MultiField,
        }
    }
}
