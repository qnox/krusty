//! Facts a Kotlin declaration states on its JVM realization rather than in its metadata.
//!
//! `@Deprecated` is a runtime annotation on the method that realizes a declaration (for a
//! property, its `$annotations` method and accessors), not part of the protobuf. The decoded
//! declarations and the classfile methods are both in hand only while a class's metadata is
//! decoded, so the facts are stamped onto the declarations here. Matching is by exact
//! (name, descriptor); a declaration without a descriptor takes no fact.

use super::{MetaConstructor, MetaFn, MetaJvmMethodSig, MetaProp};
use crate::jvm::classreader::MethodSig;
use crate::types::DeclaredDeprecation;

/// The classfile methods of the class whose metadata is being decoded.
pub(super) struct Realizations<'a> {
    methods: &'a [MethodSig],
}

impl<'a> Realizations<'a> {
    pub(super) fn new(methods: &'a [MethodSig]) -> Self {
        Self { methods }
    }

    fn method(&self, name: &str, descriptor: Option<&str>) -> Option<&'a MethodSig> {
        let descriptor = descriptor?;
        self.methods
            .iter()
            .find(|method| method.name == name && method.descriptor == descriptor)
    }

    /// `@Deprecated(level = HIDDEN)` on the realization: the declaration exists for binary
    /// compatibility only, and kotlinc drops it from resolution.
    fn hidden(&self, name: &str, descriptor: Option<&str>) -> bool {
        self.method(name, descriptor)
            .is_some_and(|method| method.deprecated_hidden)
    }

    fn deprecation(
        &self,
        name: &str,
        descriptor: Option<&str>,
    ) -> Option<Box<DeclaredDeprecation>> {
        self.method(name, descriptor)
            .and_then(|method| method.deprecation.clone())
    }

    fn accessor_deprecation(
        &self,
        accessor: Option<&MetaJvmMethodSig>,
    ) -> Option<Box<DeclaredDeprecation>> {
        accessor.and_then(|accessor| self.deprecation(&accessor.name, Some(&accessor.desc)))
    }

    pub(super) fn stamp_functions(&self, mut functions: Vec<MetaFn>) -> Vec<MetaFn> {
        for function in &mut functions {
            if self.hidden(&function.jvm_name, function.jvm_desc) {
                function.flags = function.flags.with_deprecated_hidden(true);
            }
            function.set_deprecation(self.deprecation(&function.jvm_name, function.jvm_desc));
            // Metadata records a function's annotations only under `AnnotationsInMetadata`; a
            // library compiled without it states them on the realization, where kotlinc reads them
            // (`@LowPriorityInOverloadResolution` among them). Any recorded annotation keeps
            // metadata authoritative, and `HAS_ANNOTATIONS` unset means there are none.
            if function.annotations_on_realization() {
                if let Some(method) = self.method(&function.jvm_name, function.jvm_desc) {
                    function.annotations = method.annotations.to_vec();
                }
            }
        }
        functions
    }

    pub(super) fn stamp_properties(&self, mut properties: Vec<MetaProp>) -> Vec<MetaProp> {
        for property in &mut properties {
            let annotation_method = property
                .annotation_method
                .as_ref()
                .and_then(|method| self.method(&method.name, Some(&method.desc)));
            property.deprecated_hidden =
                annotation_method.is_some_and(|method| method.deprecated_hidden);
            property.hidden_since =
                annotation_method.and_then(|method| method.deprecated_hidden_since);
            property.deprecation = self.accessor_deprecation(property.annotation_method.as_ref());
            property.getter_deprecation = self.accessor_deprecation(property.getter.as_ref());
            property.setter_deprecation = self.accessor_deprecation(property.setter.as_ref());
        }
        properties
    }

    pub(super) fn stamp_constructors(&self, constructors: &mut [MetaConstructor]) {
        for constructor in constructors {
            let method = self.method(constructor.jvm_name, constructor.jvm_desc);
            constructor.deprecated_hidden = method.is_some_and(|method| method.deprecated_hidden);
            constructor.hidden_since = method.and_then(|method| method.deprecated_hidden_since);
            constructor.deprecation = method.and_then(|method| method.deprecation.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::metadata::function::tests::blank;
    use crate::jvm::metadata::MfnFlags;
    use crate::types::{type_name, TypeName};

    const LOW_PRIORITY: &str = "kotlin/internal/LowPriorityInOverloadResolution";

    fn realization() -> MethodSig {
        MethodSig {
            access: crate::jvm::classreader::ACC_PUBLIC,
            name: "pick".to_string(),
            descriptor: "(Ljava/lang/String;)V".to_string(),
            signature: None,
            parameter_nullability: Vec::new(),
            return_nullability: None,
            deprecated_hidden: false,
            deprecated_hidden_since: None,
            deprecation: None,
            has_annotation_default: false,
            annotations: Box::new([type_name(LOW_PRIORITY)]),
        }
    }

    fn stamped(annotations_on_realization: bool) -> Vec<TypeName> {
        let mut decoded = blank();
        decoded.jvm_name = "pick".to_string();
        decoded.jvm_desc = Some("(Ljava/lang/String;)V");
        decoded.flags =
            MfnFlags::default().with_annotations_on_realization(annotations_on_realization);
        let methods = [realization()];
        let [function] = <[MetaFn; 1]>::try_from(
            Realizations::new(&methods).stamp_functions(vec![MetaFn::from_decoded(decoded)]),
        )
        .expect("one function");
        function.annotations
    }

    /// The external payload shape (`HAS_ANNOTATIONS` without records) takes the realization's.
    #[test]
    fn annotations_stated_only_on_the_realization_are_stamped() {
        assert_eq!(stamped(true), vec![type_name(LOW_PRIORITY)]);
    }

    /// Metadata stating no annotations, or recording them itself (even undecodably), stays
    /// authoritative over a physical method annotation.
    #[test]
    fn metadata_without_the_external_payload_shape_keeps_its_annotations() {
        assert_eq!(stamped(false), Vec::<TypeName>::new());
    }
}
