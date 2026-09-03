//! JVM realization facts decoded from a Kotlin value-class declaration.
//!
//! Kotlin metadata is the authority for the underlying property. Both the classpath symbol provider
//! and the JVM representation pass consume this one decoder so frontend discovery and backend
//! realization cannot drift into parallel metadata/descriptor fallbacks.

use crate::jvm::classreader::ClassInfo;
use crate::types::Ty;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ValueClassDeclaration {
    pub underlying: Ty,
    pub property: Option<String>,
}

pub(super) fn from_class_info(class: &ClassInfo) -> Option<ValueClassDeclaration> {
    let metadata = crate::jvm::metadata::class_inline(class)?;
    let underlying = match metadata.underlying_class.as_deref() {
        Some(classifier) => crate::jvm::classpath::kotlin_name_to_ty(classifier),
        None => class
            .methods
            .iter()
            .find(|method| method.name == "box-impl")
            .and_then(|method| crate::jvm::names::parse_method_descriptor(&method.descriptor))
            .and_then(|(parameters, _)| parameters.first().copied())
            .map(crate::jvm::jvm_libraries::field_desc_to_ty)
            .unwrap_or_else(|| Ty::obj("kotlin/Any")),
    };
    let underlying = if metadata.underlying_nullable == Some(false) {
        underlying
    } else {
        Ty::nullable(underlying)
    }
    .canonical_semantic();
    Some(ValueClassDeclaration {
        underlying,
        property: metadata.property_name.clone(),
    })
}
