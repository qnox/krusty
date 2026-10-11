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
