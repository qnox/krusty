//! kotlinc's `JAVA_CLASS_PROPERTY_REFERENCE`: `x::javaClass` selects the stdlib `javaClass`
//! extension property, so the reference is a property of the receiver rather than the receiver's
//! Java class. It is a warning until `ForbidJavaClassPropertyReferences`, an error after.

use super::deprecation_phase::DeprecationNames;
use super::*;

const MESSAGE: &str = "'::javaClass' returns a property reference and is error-prone. \
Use '.javaClass' or '::class.java' to access the Java class of the receiver.";

impl Checker<'_> {
    /// Reports a callable reference that selected the stdlib `javaClass` extension property, at
    /// the referenced name. A member or source property of the same name is another declaration.
    pub(super) fn report_java_class_property_reference(
        &mut self,
        expression: ExprId,
        property: &crate::symbol_resolver::ResolvedPropertyRef,
    ) {
        let language = &self.file.language;
        if !language.forbid_java_class_property_references
            && language.java_class_property_reference_warning.is_none()
        {
            return;
        }
        if property.extension_facade.is_none() || property.name != "javaClass" {
            return;
        }
        let declared_in_kotlin_jvm = matches!(
            property.getter.declaration_owner,
            Some(crate::types::SemanticCallableOwner::Package(package)) if package.matches("kotlin/jvm")
        );
        if !declared_in_kotlin_jvm {
            return;
        }
        self.report_deprecation_error(
            self.member_name_span(expression, &property.name),
            self.file.language.forbid_java_class_property_references,
            self.file
                .language
                .java_class_property_reference_warning
                .as_deref(),
            DeprecationNames {
                error: crate::diag::names::JAVA_CLASS_PROPERTY_REFERENCE_ERROR,
                warning: crate::diag::names::JAVA_CLASS_PROPERTY_REFERENCE_WARNING,
            },
            MESSAGE,
        );
    }
}
