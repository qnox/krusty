//! The primary-constructor rule kotlinc's value-class declaration checker applies to an `expect`
//! value class (`FirValueClassDeclarationChecker.ForExpectClass`).
//!
//! Actualization removes a matched `expect` classifier before checking, so these headers are
//! checked here, while every file's syntax is live. 2.4.20's `AllowExpectValueClassesWithNoPrimaryConstructor`
//! lets the `actual` supply the constructor; the class may then declare no secondary
//! constructor. Earlier releases always require a primary constructor.

use crate::ast::{ClassKind, Decl, File};
use crate::diag::DiagSink;
use crate::diagnostic_wording::value_class_without_primary_constructor;

/// Report each top-level `expect` value class that writes no primary constructor, and say
/// whether any was reported: kotlinc stops before actualization then.
pub(super) fn validate(file: &File, diagnostics: &mut DiagSink) -> bool {
    let before = diagnostics.diags.len();
    let rules = &file.language_gates.value_classes;
    for expect in &file.expect_decls {
        let Decl::Class(class) = file.decl(expect.declaration) else {
            continue;
        };
        let Some(keyword) = class.value_modifier_span else {
            continue;
        };
        if class.kind != ClassKind::Class
            || class.is_singleton()
            || class.primary_constructor_span.is_some()
        {
            continue;
        }
        // kotlinc's `finalOrInlineClassPrefix`. Under `FullValueClasses` the class is full unless
        // it applies `@JvmInline`, which only resolution can tell; a class written with
        // annotations there is not checked yet.
        let kind = if !file.full_value_classes {
            "value"
        } else if class.annotations.is_empty() {
            if !class.is_final() {
                continue;
            }
            "final value"
        } else {
            continue;
        };
        if !rules.expect_without_primary_constructor {
            diagnostics.error(keyword, value_class_without_primary_constructor(kind));
            continue;
        }
        for constructor in &class.secondary_ctors {
            diagnostics.error(
                constructor.declaration_span,
                format!(
                    "expect {kind} class without primary constructor cannot have secondary constructors."
                ),
            );
        }
    }
    diagnostics.diags.len() > before
}
