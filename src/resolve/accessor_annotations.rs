//! Annotations written on a property's accessors (`@A get()`, `@A set(v)`) and on a setter's value
//! parameter (`set(@A v)`).
//!
//! Each is an ordinary checked application resolved in the scope of the property's own
//! annotations. Its declaration is the getter, the setter or the value parameter, so the
//! annotation class must list that target: kotlinc's WRONG_ANNOTATION_TARGET otherwise, at the
//! application's `@`.

use super::*;
use crate::types::KotlinTarget;

impl Checker<'_> {
    pub(super) fn check_accessor_annotations(
        &mut self,
        scope: &CheckerScope<'_>,
        property: &PropDecl,
    ) {
        // The property's declaration policies cover its accessors.
        let policy_depth =
            self.push_declaration_policies(scope, &property.annotations, &property.annotation_args);
        self.check_accessor_annotation_site(
            scope,
            &property.getter_annotations,
            KotlinTarget::PropertyGetter,
        );
        if let Some(setter) = &property.setter {
            self.check_accessor_annotation_site(
                scope,
                &setter.annotations,
                KotlinTarget::PropertySetter,
            );
            self.check_accessor_annotation_site(
                scope,
                &setter.param_annotations,
                KotlinTarget::ValueParameter,
            );
        }
        self.active_lexical_policies.truncate(policy_depth);
    }

    fn check_accessor_annotation_site(
        &mut self,
        scope: &CheckerScope<'_>,
        annotations: &[crate::ast::AccessorAnnotation],
        target: KotlinTarget,
    ) {
        for entry in annotations {
            let Some(classifier) =
                self.check_annotation_application(scope, &entry.annotation, &entry.arguments)
            else {
                continue;
            };
            let Some(targets) = self.diagnostic_annotation_targets(classifier) else {
                continue;
            };
            if targets.allows(target) || self.suppresses_diagnostic("WRONG_ANNOTATION_TARGET") {
                continue;
            }
            let applicable = targets
                .applicable()
                .iter()
                .map(|target| target.description())
                .collect::<Vec<_>>()
                .join(", ");
            self.diags.error(
                Span::new(entry.at, entry.annotation.span.hi),
                format!(
                    "this annotation is not applicable to target '{}'. Applicable targets: \
                     {applicable}",
                    target.description()
                ),
            );
        }
    }
}
