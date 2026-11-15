//! Logical array construction, element access, and generated member rendering.

use super::{prim_array_element, prim_array_name, wk, Ty, TyKind};

impl Ty {
    /// An array whose element is `elem`, choosing the array *kind* the way Kotlin does: a primitive
    /// element yields the specialized primitive array (`Int` → `IntArray` = `Obj("kotlin/IntArray")`,
    /// `[I`), any reference element yields the boxed `Array<T>` (`String` → `Obj("kotlin/Array", [String])`,
    /// `[Ljava/lang/String;`). To force a boxed `Array<Int>` (`[Ljava/lang/Integer;`) construct it
    /// directly as `Ty::obj_args_name(wk::array(), &[Ty::Int])`.
    pub fn array(elem: Ty) -> Ty {
        match prim_array_name(elem) {
            Some(n) => Ty::obj(n),
            None => Ty::obj_args_name(wk::array(), &[elem]),
        }
    }

    /// The element type if this is an array — a primitive specialized array (`IntArray` → `Int`) or a
    /// Kotlin `Array<T>` carried as `Obj("kotlin/Array", [T])` (its *logical* element, e.g. `Int` for
    /// `Array<Int>`; the wrapper boxing is the backend's concern, not the type's).
    pub fn array_elem(self) -> Option<Ty> {
        match self.kind() {
            TyKind::Obj(n, args) if n == wk::array() => args.first().copied(),
            TyKind::Obj(n, _) => prim_array_element(n),
            TyKind::TyParam(_, b) | TyKind::Var(_, b) | TyKind::PlatformNullable(b) => {
                b.array_elem()
            }
            _ => None,
        }
    }

    /// The type produced by READING one element from this array. [`Ty::array_elem`] preserves the
    /// classifier argument exactly, including a use-site projection; an expression cannot itself
    /// have that projection wrapper. Reading `Array<out T>` yields `T`, while an `in`-projected
    /// array exposes only nullable `Any`. Primitive and invariant arrays keep their declared element.
    pub fn array_read_elem(self) -> Option<Ty> {
        Some({
            let element = self.array_elem()?;
            match element.kind() {
                TyKind::OutProjection(inner) | TyKind::StarProjection(inner) => inner,
                TyKind::InProjection(_) => Ty::nullable(Ty::obj("kotlin/Any")),
                _ => element,
            }
        })
    }

    /// Whether this type is any array — a primitive specialized array (`kotlin/IntArray`, …) or a boxed
    /// `Array<T>` (`Obj("kotlin/Array", [T])`). The single array-ness predicate; consumers must use this
    /// instead of pattern-matching a specific spelling so the representation can migrate under them.
    pub fn is_array(self) -> bool {
        matches!(self.kind(), TyKind::Obj(n, _) if n == wk::array() || prim_array_element(n).is_some())
    }

    /// Whether this is a boxed `Array<T>` (`aaload`/`aastore`, elements stored as objects) as opposed to
    /// a primitive specialized array (`IntArray` → `iaload`/`iastore`). The only bit the backend needs to
    /// pick array opcodes; a reference array boxes primitive [`array_elem`]s at the store boundary.
    pub fn is_reference_array(self) -> bool {
        matches!(self.kind(), TyKind::Obj(n, _) if n == wk::array())
    }

    /// Whether this is an unsigned specialized array (`UIntArray`, …): a value class over the
    /// signed array of its width, which Kotlin renders, compares and hashes through its own members
    /// rather than as an array.
    pub fn is_unsigned_array(self) -> bool {
        matches!(self.kind(), TyKind::Obj(n, _)
            if prim_array_element(n).is_some_and(|element| element.is_unsigned()))
    }

    /// Whether a generated `toString` renders a property of this type by its CONTENTS (`[1, 2]`),
    /// as Kotlin's data and value classes render an array property. An unsigned array is a value
    /// class and renders through its own `toString` (`UIntArray(storage=[1])`), so it is not one.
    pub fn renders_array_contents(self) -> bool {
        let ty = self.non_null();
        ty.is_array() && !ty.is_unsigned_array()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_signed_and_reference_arrays_render_by_content() {
        for name in [
            "kotlin/IntArray",
            "kotlin/LongArray",
            "kotlin/BooleanArray",
            "kotlin/CharArray",
        ] {
            assert!(Ty::obj(name).renders_array_contents(), "{name}");
            assert!(
                Ty::nullable(Ty::obj(name)).renders_array_contents(),
                "{name}?"
            );
            assert!(!Ty::obj(name).is_unsigned_array(), "{name}");
        }
        assert!(Ty::obj_args("kotlin/Array", &[Ty::String]).renders_array_contents());
        for name in [
            "kotlin/UByteArray",
            "kotlin/UShortArray",
            "kotlin/UIntArray",
            "kotlin/ULongArray",
        ] {
            let unsigned = Ty::obj(name);
            assert!(unsigned.is_array(), "{name}");
            assert!(unsigned.is_unsigned_array(), "{name}");
            assert!(!unsigned.renders_array_contents(), "{name}");
            assert!(!Ty::nullable(unsigned).renders_array_contents(), "{name}?");
        }
        assert!(!Ty::String.renders_array_contents());
    }
}
