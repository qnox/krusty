//! Federation of Kotlin builtins declarations with their mapped JVM realizations.
//!
//! A mapped classifier such as `kotlin.Throwable` has two provider inputs: Kotlin builtins metadata
//! describes its source declaration, while the JDK class describes the method that realizes it. Core
//! resolution must receive one ordinary declaration containing both views.

use crate::libraries::LibraryMember;
use crate::types::{type_name, TypeName};

const SPECIAL_REALIZATIONS_2_4: &str =
    include_str!("mapped_builtin_declarations/special_realizations_2_4.tsv");

/// Declaration kind is part of a mapped member's identity. A zero-argument function and a property
/// getter can share a descriptor without occupying the same Kotlin namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum MappedBuiltinMemberKind {
    Property,
    Function,
}

/// One metadata-declared Kotlin builtin member paired with its exact JVM realization.
///
/// Source and physical names deliberately remain separate. The declaring Kotlin identity and full
/// erased descriptor make this safe to apply to a concrete Java class hierarchy without teaching
/// that hierarchy about Kotlin collection names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MappedBuiltinMember {
    pub(super) declaration_owner: TypeName,
    pub(super) source_name: String,
    pub(super) physical_owner: TypeName,
    pub(super) physical_name: String,
    pub(super) descriptor: String,
    pub(super) kind: MappedBuiltinMemberKind,
}

impl MappedBuiltinMember {
    pub(super) fn is_property(&self) -> bool {
        self.kind == MappedBuiltinMemberKind::Property
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct SpecialRealization {
    declaration_owner: TypeName,
    source_name: &'static str,
    descriptor: &'static str,
    kind: MappedBuiltinMemberKind,
    physical_owner: TypeName,
    physical_name: &'static str,
}

fn special_realizations() -> &'static [SpecialRealization] {
    static REALIZATIONS: std::sync::OnceLock<Box<[SpecialRealization]>> =
        std::sync::OnceLock::new();
    REALIZATIONS.get_or_init(|| {
        SPECIAL_REALIZATIONS_2_4
            .lines()
            .enumerate()
            .filter_map(|(index, line)| {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    return None;
                }
                let mut fields = line.split('\t');
                let (
                    Some(declaration_owner),
                    Some(source_name),
                    Some(descriptor),
                    Some(kind),
                    Some(physical_owner),
                    Some(physical_name),
                    None,
                ) = (
                    fields.next(),
                    fields.next(),
                    fields.next(),
                    fields.next(),
                    fields.next(),
                    fields.next(),
                    fields.next(),
                )
                else {
                    panic!(
                        "invalid mapped-builtin realization policy row {}",
                        index + 1
                    );
                };
                let kind = match kind {
                    "property" => MappedBuiltinMemberKind::Property,
                    "function" => MappedBuiltinMemberKind::Function,
                    other => panic!(
                        "invalid mapped-builtin realization kind {other:?} on row {}",
                        index + 1
                    ),
                };
                Some(SpecialRealization {
                    declaration_owner: type_name(declaration_owner),
                    source_name,
                    descriptor,
                    kind,
                    physical_owner: type_name(physical_owner),
                    physical_name,
                })
            })
            .collect::<Vec<_>>()
            .into_boxed_slice()
    })
}

/// The JVM realization policy for one exact declaration decoded from `.kotlin_builtins`.
pub(super) fn realization_for_declaration(
    declaration_owner: TypeName,
    source_name: &str,
    descriptor: &str,
    kind: MappedBuiltinMemberKind,
) -> Option<(TypeName, &'static str)> {
    special_realizations()
        .iter()
        .find(|realization| {
            realization.declaration_owner == declaration_owner
                && realization.source_name == source_name
                && realization.descriptor == descriptor
                && realization.kind == kind
        })
        .map(|realization| (realization.physical_owner, realization.physical_name))
}

/// Physical spelling for an already-resolved mapped-builtin call. The owner may be either the
/// Kotlin declaration identity or its exact JVM realization; the descriptor disambiguates overloads.
pub(super) fn physical_name_for_call(
    owner: TypeName,
    source_name: &str,
    descriptor: &str,
) -> Option<&'static str> {
    special_realizations()
        .iter()
        .find(|realization| {
            (realization.declaration_owner == owner || realization.physical_owner == owner)
                && realization.source_name == source_name
                && realization.descriptor == descriptor
        })
        .map(|realization| realization.physical_name)
}

/// Whether a concrete classfile method is the realization of an exact mapped builtin property.
pub(super) fn is_property_realization(
    physical_owner: TypeName,
    source_name: &str,
    physical_name: &str,
    descriptor: &str,
) -> bool {
    special_realizations().iter().any(|realization| {
        realization.kind == MappedBuiltinMemberKind::Property
            && realization.physical_owner == physical_owner
            && realization.source_name == source_name
            && realization.physical_name == physical_name
            && realization.descriptor == descriptor
    })
}

/// Overlay source-semantic constructor facts onto the matching physical constructors.
///
/// Constructors that exist only on the JVM remain available: Kotlin's JVM builtins customizer may
/// expose platform constructors beyond the common builtins declaration. Conversely, a declaration
/// without a physical match is not published from this path because it has no JVM realization.
pub(super) fn overlay_constructor_semantics(
    physical: &mut [LibraryMember],
    semantic: Vec<LibraryMember>,
) {
    for declaration in semantic {
        let Some(realization) =
            physical.iter_mut().find(|candidate| {
                candidate.params.len() == declaration.params.len()
                    && candidate.params.iter().zip(&declaration.params).all(
                        |(&physical, &semantic)| {
                            physical.non_null().canonical_semantic()
                                == semantic.non_null().canonical_semantic()
                        },
                    )
            })
        else {
            continue;
        };

        // Keep descriptor/owner/physical types from the classfile realization. Everything below is
        // declaration semantics and therefore comes from Kotlin builtins metadata.
        realization.params = declaration.params;
        realization.ret = declaration.ret;
        realization.generic_sig = declaration.generic_sig;
        realization.visibility = declaration.visibility;
        realization.call_sig = declaration.call_sig;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        is_property_realization, overlay_constructor_semantics, physical_name_for_call,
        realization_for_declaration, special_realizations, MappedBuiltinMemberKind,
    };
    use crate::libraries::{CallSig, LibraryMember};
    use crate::types::{type_name, Ty};

    #[test]
    fn semantic_overlay_preserves_the_physical_constructor_realization() {
        let mut physical = vec![LibraryMember::new(
            "<init>".to_string(),
            vec![Ty::platform_nullable(Ty::String)],
            Ty::Unit,
            "(Ljava/lang/String;)V".to_string(),
        )];
        let mut semantic = LibraryMember::new(
            "<init>".to_string(),
            vec![Ty::nullable(Ty::String)],
            Ty::Unit,
            String::new(),
        );
        semantic.call_sig =
            CallSig::metadata_member(1, vec!["message".to_string()], vec![false], None);

        overlay_constructor_semantics(&mut physical, vec![semantic]);

        assert_eq!(physical[0].params, [Ty::nullable(Ty::String)]);
        assert_eq!(
            physical[0].physical_params,
            [Ty::platform_nullable(Ty::String)]
        );
        assert_eq!(physical[0].call_sig.param_names, ["message"]);
        assert_eq!(physical[0].descriptor, "(Ljava/lang/String;)V");
    }

    #[test]
    fn special_realization_policy_is_exact_and_identity_qualified() {
        let rows = special_realizations()
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(special_realizations().len(), 8);
        assert_eq!(rows.len(), special_realizations().len());

        assert_eq!(
            realization_for_declaration(
                type_name("kotlin/collections/Map"),
                "keys",
                "()Ljava/util/Set;",
                MappedBuiltinMemberKind::Property,
            ),
            Some((type_name("java/util/Map"), "keySet"))
        );
        assert_eq!(
            realization_for_declaration(
                type_name("kotlin/collections/Set"),
                "keys",
                "()Ljava/util/Set;",
                MappedBuiltinMemberKind::Property,
            ),
            None,
            "a same-named property on another declaration must not inherit Map's policy"
        );
        assert_eq!(
            realization_for_declaration(
                type_name("kotlin/collections/MutableList"),
                "removeAt",
                "(Ljava/lang/Object;)Ljava/lang/Object;",
                MappedBuiltinMemberKind::Function,
            ),
            None,
            "a same-named overload must not inherit removeAt(Int)'s policy"
        );
        assert_eq!(
            physical_name_for_call(
                type_name("java/util/List"),
                "removeAt",
                "(I)Ljava/lang/Object;",
            ),
            Some("remove")
        );
        assert!(is_property_realization(
            type_name("java/util/Map"),
            "entries",
            "entrySet",
            "()Ljava/util/Set;",
        ));
        assert!(!is_property_realization(
            type_name("fixtures/MapLike"),
            "entries",
            "entrySet",
            "()Ljava/util/Set;",
        ));
    }
}
