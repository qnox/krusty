//! Compiler plugins' frontend class rules.
//!
//! kotlinc's plugins register FIR checkers of their own: kotlinx.serialization rejects a
//! `@Transient` property without an initializer before any code is generated. krusty runs the
//! same rules here, on the class being checked, so a class a plugin rejects is reported with the
//! plugin's diagnostic and never reaches backend generation.
//!
//! The plugin sees resolved annotation identities, read from the applications this checker has
//! just validated: an import alias or a `typealias` of an annotation is the annotation it names,
//! and an unrelated annotation class with the same simple name is not. This runs while the
//! class's syntax is live, so nothing about the declaration has to survive syntax release to be
//! checked.

use super::*;
use crate::plugins::{FrontendClassCheckContext, FrontendPropertyFacts};

impl Checker<'_> {
    pub(super) fn check_plugin_class_rules(&mut self, scope: &CheckerScope<'_>, cl: &ClassDecl) {
        // Only the authoritative pass reports: capture discovery, type inference on demand and
        // the restricted passes re-enter a class without owning its diagnostics.
        if self.native_plugins.is_empty()
            || self.discover_anonymous_captures
            || self.inference_only
            || !matches!(self.fragment, SourceFragmentMode::Complete)
        {
            return;
        }
        let mut annotations = Vec::with_capacity(cl.annotations.len());
        let mut annotation_class_arguments = Vec::new();
        for annotation in &cl.annotations {
            let Some(identity) = self.annotation_identity_in_scope(scope, annotation) else {
                continue;
            };
            let ordinal = u32::try_from(annotations.len()).expect("annotation count fits u32");
            annotations.push(identity);
            if let Some(applied) = self
                .applied_annotations
                .get(&(annotation.span.lo, annotation.span.hi))
            {
                annotation_class_arguments.extend(applied.values.iter().filter_map(
                    |(_, value)| match value {
                        crate::types::AnnotationValue::Class(classifier) => {
                            Some((ordinal, *classifier))
                        }
                        _ => None,
                    },
                ));
            }
        }
        let constructor_properties = cl
            .props
            .iter()
            .filter(|parameter| parameter.is_property)
            .map(|parameter| FrontendPropertyFacts {
                annotations: self.annotation_identities(scope, &parameter.annotations),
                has_backing_field: true,
                has_initializer: parameter.default.is_some(),
                is_lateinit: false,
                declaration_span: parameter.declaration_span,
            })
            .collect::<Vec<_>>();
        let properties = constructor_properties
            .into_iter()
            .chain(cl.body_props.iter().map(|property| FrontendPropertyFacts {
                annotations: self.annotation_identities(scope, &property.annotations),
                has_backing_field: !property.is_abstract
                    && !property.is_external
                    && property.delegate.is_none()
                    && (property.getter.is_none()
                        || property.getter_reads_field
                        || property.init.is_some()
                        || property.explicit_backing_field.is_some()),
                has_initializer: property.init.is_some(),
                is_lateinit: property.is_lateinit,
                declaration_span: property.declaration_span,
            }))
            .collect::<Vec<_>>();
        let kind = if cl.is_annotation() {
            crate::libraries::TypeKind::Annotation
        } else if cl.is_singleton() {
            crate::libraries::TypeKind::Object
        } else if cl.is_enum() {
            crate::libraries::TypeKind::Enum
        } else if cl.is_interface() {
            crate::libraries::TypeKind::Interface
        } else {
            crate::libraries::TypeKind::Class
        };
        let context = FrontendClassCheckContext {
            kind,
            annotations: &annotations,
            annotation_class_arguments: &annotation_class_arguments,
            properties: &properties,
        };
        for diagnostic in self
            .native_plugins
            .host("main")
            .check_frontend_class(&context)
        {
            self.diags.error(diagnostic.span, diagnostic.message);
        }
    }

    fn annotation_identities(
        &self,
        scope: &CheckerScope<'_>,
        annotations: &[AnnotationRef],
    ) -> Vec<TypeName> {
        annotations
            .iter()
            .filter_map(|annotation| self.annotation_identity_in_scope(scope, annotation))
            .collect()
    }
}
