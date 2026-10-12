//! `external` on an enum entry or a primary-constructor property, kotlinc's modifier-target rule
//! for `ForbidExternalEnumEntriesAndPrimaryConstructorProperties` (KTLC-389): deprecated while
//! the feature is off, not applicable once it is on. The parser records each such keyword in
//! [`File::external_modifier_uses`].

use crate::ast::File;
use crate::diag::DiagSink;
use crate::features::LangFeatures;

const FEATURE: &str = "ForbidExternalEnumEntriesAndPrimaryConstructorProperties";

/// Report every `external` keyword written on a deprecated target, at the keyword.
pub(super) fn check(file: &File, features: &LangFeatures, diagnostics: &mut DiagSink) {
    let forbidden = features.has(FEATURE);
    let mut reported = Vec::new();
    for modifier in &file.external_modifier_uses {
        if reported.contains(&modifier.span) {
            continue;
        }
        reported.push(modifier.span);
        if modifier.repeated {
            diagnostics.error(modifier.span, "repeated 'external'.");
        } else if forbidden {
            let target = modifier.target.description();
            diagnostics.error(
                modifier.span,
                format!("modifier 'external' is not applicable to '{target}'."),
            );
        } else {
            let target = modifier.target.description();
            diagnostics.warning(
                modifier.span,
                format!("modifier 'external' is deprecated for '{target}'."),
            );
        }
    }
}
