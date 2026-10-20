//! The structural view of a [`Ty`].
//!
//! Code that takes a type apart matches on [`Ty::kind`] instead of on `Ty` itself. A `TyKind`
//! carries its component types by value, so a match never depends on how `Ty` stores them.

use super::{FnSig, Ty, TyList, TyParamName, TypeName};

/// One layer of a [`Ty`], with its component types by value. Mirrors the variants of `Ty`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum TyKind {
    Unit,
    Obj(TypeName, TyList),
    Null,
    Nothing,
    Error,
    Pending,
    Fun(&'static FnSig),
    Nullable(Ty),
    PlatformNullable(Ty),
    InProjection(Ty),
    OutProjection(Ty),
    StarProjection(Ty),
    TyParam(TyParamName, Ty),
    DefinitelyNotNull(Ty),
    Intersection(TyList),
}

impl Ty {
    /// This type's outermost layer, for matching.
    pub fn kind(self) -> TyKind {
        match self {
            Ty::Unit => TyKind::Unit,
            Ty::Obj(name, args) => TyKind::Obj(name, args),
            Ty::Null => TyKind::Null,
            Ty::Nothing => TyKind::Nothing,
            Ty::Error => TyKind::Error,
            Ty::Pending => TyKind::Pending,
            Ty::Fun(signature) => TyKind::Fun(signature),
            Ty::Nullable(inner) => TyKind::Nullable(*inner),
            Ty::PlatformNullable(inner) => TyKind::PlatformNullable(*inner),
            Ty::InProjection(inner) => TyKind::InProjection(*inner),
            Ty::OutProjection(inner) => TyKind::OutProjection(*inner),
            Ty::StarProjection(inner) => TyKind::StarProjection(*inner),
            Ty::TyParam(name, bound) => TyKind::TyParam(name, *bound),
            Ty::DefinitelyNotNull(inner) => TyKind::DefinitelyNotNull(*inner),
            Ty::Intersection(components) => TyKind::Intersection(components),
        }
    }

    /// Whether this is a function type.
    pub fn is_fun(self) -> bool {
        matches!(self.kind(), TyKind::Fun(_))
    }

    /// Whether this is a Java platform type `T!`.
    pub fn is_platform_nullable(self) -> bool {
        matches!(self.kind(), TyKind::PlatformNullable(_))
    }

    /// Whether this is a star projection `*`.
    pub fn is_star_projection(self) -> bool {
        matches!(self.kind(), TyKind::StarProjection(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kind_exposes_the_component_types_by_value() {
        let string = Ty::obj("kotlin/String");
        assert_eq!(Ty::nullable(string).kind(), TyKind::Nullable(string));
        assert_eq!(Ty::Unit.kind(), TyKind::Unit);
        assert!(matches!(string.kind(), TyKind::Obj(_, args) if args.is_empty()));
    }
}
