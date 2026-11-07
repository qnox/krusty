//! Qualified identities that Kotlin recognizes as Java nullability qualifiers.
//!
//! Java source headers and classfiles are separate providers. Both resolve an annotation to one
//! interned JVM classifier identity before consulting this table, then publish the same
//! [`JavaNullability`] fact. Descriptor parsing stays at the classfile boundary; source spellings
//! stay in the Java parser until the source-stub provider resolves them through its imports.

use std::sync::OnceLock;

use crate::types::{existing_type_name, type_name, TypeName};

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
    qualifiers()
        .iter()
        .find_map(|&(qualifier, facts)| (qualifier == identity).then_some(facts))
}

/// The recognized qualifier identities, interned once. A classfile reader asks about every
/// annotation of every member, so the table must not re-intern its own names on each question.
fn qualifiers() -> &'static [(TypeName, NullabilityQualifier); 4] {
    static QUALIFIERS: OnceLock<[(TypeName, NullabilityQualifier); 4]> = OnceLock::new();
    QUALIFIERS.get_or_init(|| {
        let qualifier = |internal, nullability, permits_declaration_site| {
            (
                type_name(internal),
                NullabilityQualifier {
                    nullability,
                    permits_declaration_site,
                },
            )
        };
        [
            qualifier(
                "org/jetbrains/annotations/NotNull",
                JavaNullability::NotNull,
                true,
            ),
            qualifier(
                "org/jetbrains/annotations/Nullable",
                JavaNullability::Nullable,
                true,
            ),
            qualifier(
                "org/checkerframework/checker/nullness/qual/NonNull",
                JavaNullability::NotNull,
                false,
            ),
            qualifier(
                "org/checkerframework/checker/nullness/qual/Nullable",
                JavaNullability::Nullable,
                false,
            ),
        ]
    })
}

/// Resolve a classfile annotation descriptor into the same qualified identity used by the Java
/// source provider. A descriptor is external JVM serialization, so this text conversion belongs at
/// this boundary and never becomes semantic lookup input afterward. The qualifier identities are
/// interned before the descriptor is looked up, so a descriptor whose identity was never interned
/// names none of them and is answered without interning it.
pub(crate) fn descriptor_nullability_qualifier(descriptor: &str) -> Option<NullabilityQualifier> {
    let internal = descriptor.strip_prefix('L')?.strip_suffix(';')?;
    let qualifiers = qualifiers();
    let identity = existing_type_name(internal)?;
    qualifiers
        .iter()
        .find_map(|&(qualifier, facts)| (qualifier == identity).then_some(facts))
}

#[cfg(test)]
mod tests {
    use super::{descriptor_nullability_qualifier, JavaNullability};
    use crate::types::existing_type_name;

    #[test]
    fn descriptors_resolve_to_the_recognized_qualifiers_without_interning_others() {
        let recognized = [
            (
                "Lorg/jetbrains/annotations/NotNull;",
                JavaNullability::NotNull,
                true,
            ),
            (
                "Lorg/jetbrains/annotations/Nullable;",
                JavaNullability::Nullable,
                true,
            ),
            (
                "Lorg/checkerframework/checker/nullness/qual/NonNull;",
                JavaNullability::NotNull,
                false,
            ),
            (
                "Lorg/checkerframework/checker/nullness/qual/Nullable;",
                JavaNullability::Nullable,
                false,
            ),
        ];
        for (descriptor, nullability, permits_declaration_site) in recognized {
            let qualifier = descriptor_nullability_qualifier(descriptor).expect(descriptor);
            assert_eq!(qualifier.nullability, nullability, "{descriptor}");
            assert_eq!(
                qualifier.permits_declaration_site, permits_declaration_site,
                "{descriptor}"
            );
        }

        let unrecognized = "nullability/qualifier/test/NeverInterned";
        assert!(descriptor_nullability_qualifier(&format!("L{unrecognized};")).is_none());
        assert_eq!(existing_type_name(unrecognized), None);
        assert!(descriptor_nullability_qualifier("I").is_none());
    }
}
