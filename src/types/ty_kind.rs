//! The structural view of a [`Ty`].
//!
//! Code that takes a type apart matches on [`Ty::kind`] instead of on `Ty` itself. A `TyKind`
//! carries its component types by value; `Ty` itself is only an id into the type table.

use super::{FnSig, Ty, TyList, TyParamName, TypeName};

/// One layer of a [`Ty`], with its component types by value.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum TyKind {
    Unit,
    /// A reference type by internal name (e.g. `demo/Point`), with its generic type arguments
    /// (`List<Int>` → `Obj("kotlin/collections/List", [Int])`). Arguments are interned (`intern_tys`)
    /// so equal instantiations share a pointer and the front end can recover element/member types.
    /// Empty for a non-generic class.
    Obj(TypeName, TyList),
    /// The type of the `null` literal — assignable only to nullable types.
    Null,
    /// The bottom type (`Nothing`): the type of `throw`/`return` expressions. Assignable to every
    /// type; an expression of this type never yields a value (it always diverges).
    Nothing,
    /// Placeholder after a type error, suppresses cascading diagnostics.
    Error,
    /// A declaration whose type is NOT DETERMINED YET — recorded by signature collection for an
    /// implicitly-typed declaration the resolution engine has not resolved.
    ///
    /// Distinct from [`TyKind::Error`] on purpose. Spelling "not yet" and "gave up" the same way is what
    /// forces every read site to be taught the difference by hand: a reader that finds `Error`
    /// cannot know whether asking the engine would help, so each place has to be patched
    /// individually and any place that is missed silently publishes a wrong type. `Pending` is
    /// answerable — a read of it demands the declaration, and it must never reach emission or
    /// `@Metadata`, which is an invariant that can be asserted rather than hoped for.
    Pending,
    /// A Kotlin function type `(A, B) -> R`. The front end keeps the real parameter/return types
    /// (interned `FnSig`) so a call through a `Fun` value recovers its return type.
    Fun(&'static FnSig),
    /// A nullable type `T?`. Wraps the interned non-null type. Kotlin has no `T??`, so the inner type
    /// is never itself `Nullable` (the [`Ty::nullable`] constructor enforces this).
    Nullable(Ty),
    /// A Java platform type `T!`: flexible between `T` and `T?`. Unlike [`Nullable`](TyKind::Nullable),
    /// direct member access is permitted; assignability accepts it at either nullability bound.
    PlatformNullable(Ty),
    /// Use-site type projections. These occur only as generic arguments (`Box<in T>`, `Box<out T>`,
    /// and `Box<*>`) and remain semantic until assignability/inference consumes their variance. A
    /// star retains its readable upper bound, but stays distinct from an explicitly written `out`
    /// projection because Kotlin metadata and JVM generic signatures encode those forms differently.
    InProjection(Ty),
    OutProjection(Ty),
    StarProjection(Ty),
    /// A generic type-parameter reference (`T`), carrying its name and declared upper bound
    /// (`<T : CharSequence>` → bound `CharSequence`; unbounded `<T>` → bound `kotlin/Any`). The checker
    /// reasons about `T` as `T` (subtyping against the bound, substitution at instantiation); runtime
    /// erasure is a backend concern.
    TyParam(TyParamName, Ty),
    /// Kotlin's definitely-non-null type `T & Any`.
    ///
    /// The wrapped type is the original type parameter, including a bound that still admits null.
    /// This is distinct from a parameter declared `T : Any`: `(T & Any)?` reopens to that original
    /// `T?`, while `T : Any` keeps its recorded bound when used as `T?`.
    DefinitelyNotNull(Ty),
    /// A denotable intersection `A & B & …` produced when a type variable's only constraints are
    /// incompatible upper bounds (`Int & String`). It is a subtype of each component and is not
    /// [`TyKind::Nothing`]: the value still exists at run time.
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
        Ty::intern(kind)
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
