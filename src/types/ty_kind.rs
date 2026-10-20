//! The structural view of a [`Ty`].
//!
//! Code that takes a type apart matches on [`Ty::kind`] instead of on `Ty` itself. A `TyKind`
//! carries its component types by value, so a match never depends on how `Ty` stores them.

use super::{intern_ty, FnSig, Ty, TyList, TyParamName, TypeName};

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

/// The built-in classifier types, as kinds: `TyKind::INT` is the kind of [`Ty::Int`], so a match on
/// a kind can name them alongside its structural arms.
impl TyKind {
    pub const BOOLEAN: TyKind = TyKind::Obj(super::KOTLIN_BOOLEAN, TyList::EMPTY);
    pub const BYTE: TyKind = TyKind::Obj(super::KOTLIN_BYTE, TyList::EMPTY);
    pub const SHORT: TyKind = TyKind::Obj(super::KOTLIN_SHORT, TyList::EMPTY);
    pub const INT: TyKind = TyKind::Obj(super::KOTLIN_INT, TyList::EMPTY);
    pub const LONG: TyKind = TyKind::Obj(super::KOTLIN_LONG, TyList::EMPTY);
    pub const CHAR: TyKind = TyKind::Obj(super::KOTLIN_CHAR, TyList::EMPTY);
    pub const FLOAT: TyKind = TyKind::Obj(super::KOTLIN_FLOAT, TyList::EMPTY);
    pub const DOUBLE: TyKind = TyKind::Obj(super::KOTLIN_DOUBLE, TyList::EMPTY);
    pub const UBYTE: TyKind = TyKind::Obj(super::KOTLIN_UBYTE, TyList::EMPTY);
    pub const USHORT: TyKind = TyKind::Obj(super::KOTLIN_USHORT, TyList::EMPTY);
    pub const UINT: TyKind = TyKind::Obj(super::KOTLIN_UINT, TyList::EMPTY);
    pub const ULONG: TyKind = TyKind::Obj(super::KOTLIN_ULONG, TyList::EMPTY);
    pub const STRING: TyKind = TyKind::Obj(super::KOTLIN_STRING, TyList::EMPTY);
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

impl From<TyKind> for Ty {
    /// The type with this outermost layer: the inverse of [`Ty::kind`].
    fn from(kind: TyKind) -> Ty {
        match kind {
            TyKind::Unit => Ty::Unit,
            TyKind::Obj(name, args) => Ty::Obj(name, args),
            TyKind::Null => Ty::Null,
            TyKind::Nothing => Ty::Nothing,
            TyKind::Error => Ty::Error,
            TyKind::Pending => Ty::Pending,
            TyKind::Fun(signature) => Ty::Fun(signature),
            TyKind::Nullable(inner) => Ty::Nullable(intern_ty(inner)),
            TyKind::PlatformNullable(inner) => Ty::PlatformNullable(intern_ty(inner)),
            TyKind::InProjection(inner) => Ty::InProjection(intern_ty(inner)),
            TyKind::OutProjection(inner) => Ty::OutProjection(intern_ty(inner)),
            TyKind::StarProjection(inner) => Ty::StarProjection(intern_ty(inner)),
            TyKind::TyParam(name, bound) => Ty::TyParam(name, intern_ty(bound)),
            TyKind::DefinitelyNotNull(inner) => Ty::DefinitelyNotNull(intern_ty(inner)),
            TyKind::Intersection(components) => Ty::Intersection(components),
        }
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
        assert_eq!(Ty::from(TyKind::Nullable(string)), Ty::nullable(string));
        assert!(matches!(string.kind(), TyKind::Obj(_, args) if args.is_empty()));
        assert_eq!(Ty::Int.kind(), TyKind::INT);
        assert_eq!(Ty::String.kind(), TyKind::STRING);
    }
}
