//! kotlinc's checks on a `data class` whose primary constructor is not public. Its generated `copy()`
//! is public, so it exposes that constructor; `DataClassCopyRespectsConstructorVisibility` (the
//! `-Xconsistent-data-class-copy-visibility` flag) gives `copy()` the constructor's visibility instead.
//! Until then, `ErrorAboutDataClassCopyVisibilityChange` decides whether two deprecations are
//! warnings or errors:
//! - `FirDataClassNonPublicConstructorChecker` reports `DATA_CLASS_COPY_VISIBILITY_WILL_BE_CHANGED`
//!   on the constructor's visibility modifier, unless the class is annotated
//!   `@ConsistentCopyVisibility` or `@ExposedCopyVisibility`;
//! - `FirDataClassCopyUsageWillBecomeInaccessibleChecker` reports `DATA_CLASS_INVISIBLE_COPY_USAGE`
//!   on the callee name of a call or callable reference to that `copy()` from a site where the
//!   constructor is not visible, whatever the class's annotations say.

use super::*;

use crate::features::deprecation_cycle_messages::{
    DATA_CLASS_COPY_VISIBILITY, DATA_CLASS_INVISIBLE_COPY_USAGE,
};

impl Checker<'_> {
    /// Check the declaration of the class `class`.
    pub(super) fn check_data_class_copy_visibility(
        &mut self,
        scope: &CheckerScope<'_>,
        class: &ClassDecl,
    ) {
        if self.file.data_copy_respects_ctor_visibility
            || !class.is_data
            || class.singleton
            || class.primary_ctor_visibility == Visibility::Public
        {
            return;
        }
        let Some(span) = class.primary_ctor_visibility_span else {
            return;
        };
        let annotated = class.annotations.iter().any(|annotation| {
            self.annotation_identity_in_scope(scope, annotation)
                .is_some_and(|identity| {
                    identity.matches("kotlin/ConsistentCopyVisibility")
                        || identity.matches("kotlin/ExposedCopyVisibility")
                })
        });
        if annotated {
            return;
        }
        self.report_deprecation_error(
            span,
            self.file.error_about_data_class_copy_visibility_change,
            self.file
                .data_class_copy_visibility_change_warning
                .as_deref(),
            crate::diag::names::DATA_CLASS_COPY_VISIBILITY_WILL_BE_CHANGED_WARNING,
            DATA_CLASS_COPY_VISIBILITY,
        );
    }

    /// Check a call or callable reference whose callee name is at `span` and which selected the
    /// visible member `member`.
    pub(super) fn check_data_class_copy_usage(
        &mut self,
        span: Span,
        member: &crate::libraries::LibraryMember,
    ) {
        if self.file.data_copy_respects_ctor_visibility {
            return;
        }
        let source_copy = member
            .stable_declaration
            .zip(self.resolved_index)
            .and_then(|(declaration, index)| index.declaration_header(declaration))
            .is_some_and(|header| {
                header
                    .flags
                    .has(crate::fir::DeclarationFlags::DATA_CLASS_COPY)
            });
        if !source_copy
            && member.semantic_role != Some(crate::types::SemanticCallRole::DataClassCopy)
        {
            return;
        }
        let Some(owner) = member.owner else {
            return;
        };
        let Some(class) = self.fed_source().classifier(owner) else {
            return;
        };
        if !class.is_data || class.is_object() {
            return;
        }
        let Some(constructor) = class
            .constructors
            .iter()
            .find(|constructor| constructor.is_primary_constructor())
        else {
            return;
        };
        let visible =
            self.callable_accessible(constructor.visibility, owner, constructor.external_identity);
        if visible {
            return;
        }
        self.report_deprecation_error(
            span,
            self.file.error_about_data_class_copy_visibility_change,
            self.file.data_class_invisible_copy_usage_warning.as_deref(),
            crate::diag::names::DATA_CLASS_INVISIBLE_COPY_USAGE_WARNING,
            DATA_CLASS_INVISIBLE_COPY_USAGE,
        );
    }
}
