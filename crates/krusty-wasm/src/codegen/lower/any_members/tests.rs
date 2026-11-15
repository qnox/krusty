use super::*;
use crate::encode::{Code, Module};
use crate::hashing::Hashing;

const SCALARS: [Ty; 8] = [
    Ty::Boolean,
    Ty::Byte,
    Ty::Short,
    Ty::Char,
    Ty::Int,
    Ty::Long,
    Ty::Float,
    Ty::Double,
];

const ANY_ROLES: [(SemanticCallRole, AnyMember); 3] = [
    (SemanticCallRole::KotlinAnyEquals, AnyMember::Equals),
    (SemanticCallRole::KotlinAnyHashCode, AnyMember::HashCode),
    (SemanticCallRole::KotlinAnyToString, AnyMember::ToString),
];

fn primitive_hash() -> Option<CompilerIntrinsic> {
    Some(CompilerIntrinsic::PrimitiveHashCode)
}

#[test]
fn every_scalar_hash_operation_is_hash_code_with_a_complete_static_realization() {
    let mut module = Module::default();
    let hashing = Hashing::declare(&mut module);
    for ty in SCALARS {
        assert_eq!(
            member_of(None, None, primitive_hash(), Some(ty)),
            Some(AnyMember::HashCode),
            "{ty:?}"
        );
        assert!(
            hashing.emit_scalar_hash(&mut Code::default(), ty),
            "{ty:?} has no scalar hash"
        );
    }
    for ty in [Ty::String, Ty::Unit, Ty::obj_name(krusty::types::wk::any())] {
        assert!(
            !hashing.emit_scalar_hash(&mut Code::default(), ty),
            "{ty:?} is not a scalar"
        );
    }
}

#[test]
fn a_call_without_a_role_or_the_hash_operation_is_no_member() {
    for receiver in [Some(Ty::Int), Some(Ty::String), None] {
        assert_eq!(member_of(None, None, None, receiver), None, "{receiver:?}");
    }
    assert_eq!(member_of(None, None, primitive_hash(), None), None);
}

#[test]
fn contradictory_roles_are_no_member() {
    for (edge, _) in ANY_ROLES {
        for (declared, _) in ANY_ROLES {
            if edge != declared {
                assert_eq!(
                    member_of(Some(edge), Some(declared), None, Some(Ty::Int)),
                    None,
                    "edge {edge:?} against declaration {declared:?}"
                );
            }
        }
    }
    for (role, _) in [ANY_ROLES[0], ANY_ROLES[2]] {
        assert_eq!(
            member_of(Some(role), None, primitive_hash(), Some(Ty::Int)),
            None,
            "{role:?} contradicts the hash operation"
        );
    }
    assert_eq!(
        member_of(
            Some(SemanticCallRole::KotlinComparableCompareTo),
            None,
            primitive_hash(),
            Some(Ty::Int)
        ),
        None,
        "a role that is not one of Any's members"
    );
}

#[test]
fn the_edge_and_the_declaration_each_carry_the_role() {
    for (role, member) in ANY_ROLES {
        for (edge, declared) in [
            (Some(role), None),
            (None, Some(role)),
            (Some(role), Some(role)),
        ] {
            assert_eq!(
                member_of(edge, declared, None, Some(Ty::String)),
                Some(member),
                "{edge:?} / {declared:?}"
            );
        }
    }
}

#[test]
fn only_the_typed_role_admits_a_string_receiver() {
    assert_eq!(
        member_of(
            Some(SemanticCallRole::KotlinAnyHashCode),
            None,
            None,
            Some(Ty::String)
        ),
        Some(AnyMember::HashCode)
    );
    assert_eq!(
        member_of(None, None, primitive_hash(), Some(Ty::String)),
        None
    );
}

/// `Any`, nullable and `Unit` receivers once declined at admission. The typed role now admits them,
/// and emission realizes what the runtime has (the dynamic hash, null's `0`, the `Unit` instance;
/// `anyMembersOfLibraryDeclarations.kt` runs those forms against kotlinc) and declines the rest
/// cleanly: `Any`'s own `toString` rendering has no body on this target yet.
#[test]
fn the_typed_role_admits_any_nullable_and_unit_receivers() {
    for receiver in [
        Ty::obj_name(krusty::types::wk::any()),
        Ty::nullable(Ty::String),
        Ty::nullable(Ty::Int),
        Ty::Unit,
    ] {
        for (role, member) in ANY_ROLES {
            assert_eq!(
                member_of(Some(role), None, None, Some(receiver)),
                Some(member),
                "{role:?} on {receiver:?}"
            );
        }
        assert_eq!(
            member_of(None, None, primitive_hash(), Some(receiver)),
            None,
            "the scalar operation does not admit {receiver:?}"
        );
    }
}
