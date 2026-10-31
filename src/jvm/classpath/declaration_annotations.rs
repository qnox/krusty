//! Declaration annotation queries over the selected classpath class file.
//!
//! Classreader owns decoding; JVM symbol providers own annotation interpretation. These queries
//! borrow the existing classpath and leave its class/field records and caches unchanged.

use super::Classpath;
use crate::jvm::classreader::{self, MethodAnnotations};
use crate::types::{ResolvedAnnotation, TypeName};

struct DeclarationAnnotations<'a> {
    classpath: &'a Classpath,
}

impl DeclarationAnnotations<'_> {
    fn class_bytes(&self, internal: TypeName) -> Option<Vec<u8>> {
        let internal = crate::jvm::jvm_class_map::to_jvm_type_name(internal);
        self.classpath
            .class_bytes(crate::jvm::names::classfile_internal_name_of(internal))
    }

    fn method(
        &self,
        internal: TypeName,
        name: &str,
        descriptor: &str,
    ) -> Option<Vec<ResolvedAnnotation>> {
        let bytes = self.class_bytes(internal)?;
        classreader::read_method_annotations(&bytes, name, descriptor)
    }

    fn methods(&self, internal: TypeName) -> Option<Vec<MethodAnnotations>> {
        let bytes = self.class_bytes(internal)?;
        classreader::method_annotations(&bytes).ok()
    }
}

impl Classpath {
    /// Annotations and their arguments on the exact named method of `internal`.
    pub fn method_annotations(
        &self,
        internal: TypeName,
        name: &str,
        descriptor: &str,
    ) -> Option<Vec<ResolvedAnnotation>> {
        DeclarationAnnotations { classpath: self }.method(internal, name, descriptor)
    }

    /// Annotations and their arguments on each method of `internal`, read from its class file.
    /// Returns `None` when the class has no readable class file.
    pub(in crate::jvm) fn class_method_annotations(
        &self,
        internal: TypeName,
    ) -> Option<Vec<MethodAnnotations>> {
        DeclarationAnnotations { classpath: self }.methods(internal)
    }
}
