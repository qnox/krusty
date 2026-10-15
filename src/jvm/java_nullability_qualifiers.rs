//! Qualified identities that Kotlin recognizes as Java nullability qualifiers.
//!
//! Java source headers and classfiles are separate providers. Both resolve an annotation to one
//! interned JVM classifier identity before consulting this table, then publish the same
//! [`JavaNullability`] fact. Descriptor parsing stays at the classfile boundary; source spellings
//! stay in the Java parser until the source-stub provider resolves them through its imports.

use crate::types::{type_name, TypeName};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JavaNullability {
    NotNull,
    Nullable,
}

/// A recognized qualifier and whether its Java declaration permits declaration-site use. Checker
/// Framework's qualifiers are type-use-only; JetBrains qualifiers also apply to member/parameter
/// declarations, which matters when an array component and its declaration have different paths.
#[derive(Clone, Copy)]
pub(crate) struct NullabilityQualifier {
    pub(crate) nullability: JavaNullability,
    pub(crate) permits_declaration_site: bool,
}

pub(crate) fn nullability_qualifier(identity: TypeName) -> Option<NullabilityQualifier> {
    if identity == type_name("org/jetbrains/annotations/NotNull") {
        Some(NullabilityQualifier {
            nullability: JavaNullability::NotNull,
            permits_declaration_site: true,
        })
    } else if identity == type_name("org/jetbrains/annotations/Nullable") {
        Some(NullabilityQualifier {
            nullability: JavaNullability::Nullable,
            permits_declaration_site: true,
        })
    } else if identity == type_name("org/checkerframework/checker/nullness/qual/NonNull") {
        Some(NullabilityQualifier {
            nullability: JavaNullability::NotNull,
            permits_declaration_site: false,
        })
    } else if identity == type_name("org/checkerframework/checker/nullness/qual/Nullable") {
        Some(NullabilityQualifier {
            nullability: JavaNullability::Nullable,
            permits_declaration_site: false,
        })
    } else {
        None
    }
}

/// Resolve a classfile annotation descriptor into the same qualified identity used by the Java
/// source provider. A descriptor is external JVM serialization, so this text conversion belongs at
/// this boundary and never becomes semantic lookup input afterward.
pub(crate) fn descriptor_nullability_qualifier(descriptor: &str) -> Option<NullabilityQualifier> {
    let internal = descriptor.strip_prefix('L')?.strip_suffix(';')?;
    nullability_qualifier(type_name(internal))
}
