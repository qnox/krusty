//! The checker's erased overload identity: the Kotlin-level key two value-parameter types share
//! when their declarations would clash after erasure. It mirrors the JVM-relevant distinctions
//! without formatting descriptors.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum ErasedTypeKey {
    Ty(Ty),
    Function(usize),
}

fn erased_key_ty(key: ErasedTypeKey) -> Ty {
    match key {
        ErasedTypeKey::Ty(t) => t,
        ErasedTypeKey::Function(n) => Ty::obj(&format!("kotlin/Function{n}")),
    }
}

/// Kotlin-level erasure key used by the checker for overload identity. This mirrors the JVM-relevant
/// distinctions without formatting descriptors: generic arguments and nullability are erased, unsigned
/// inline primitives fold to their signed representation, nullable primitives box, and type parameters
/// erase to their bound.
pub(super) fn erased_type_key(t: Ty) -> ErasedTypeKey {
    let key = match t.kind() {
        _ if t.is_unsigned() => t.scalar_value_repr().unwrap(),
        TyKind::STRING => Ty::obj("kotlin/String"),
        TyKind::Obj(n, args) if n.matches("kotlin/Array") => {
            let e = args
                .first()
                .copied()
                .unwrap_or_else(|| Ty::obj("kotlin/Any"));
            Ty::obj_args("kotlin/Array", &[erased_key_ty(erased_type_key(e))])
        }
        TyKind::Obj(n, _) => Ty::obj_name(n),
        TyKind::Null | TyKind::Nothing | TyKind::Error => Ty::obj("kotlin/Any"),
        TyKind::Fun(s) => return ErasedTypeKey::Function(s.params.len() + usize::from(s.suspend)),
        TyKind::Nullable(inner) => match inner {
            Ty::UInt => Ty::obj("kotlin/UInt"),
            Ty::ULong => Ty::obj("kotlin/ULong"),
            other if other.boxed_ref().is_some() => other.boxed_ref().unwrap_or(other),
            other => erased_key_ty(erased_type_key(other)),
        },
        TyKind::TyParam(_, bound) | TyKind::Var(_, bound) => erased_key_ty(erased_type_key(bound)),
        _ => t,
    };
    ErasedTypeKey::Ty(key)
}

pub(super) fn same_erased_params(left: &[Ty], right: &[Ty]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(&left, &right)| erased_type_key(left) == erased_type_key(right))
}

pub(super) fn erased_params_semantic_key(sig: &Signature) -> Vec<ErasedTypeKey> {
    sig.params.iter().copied().map(erased_type_key).collect()
}
