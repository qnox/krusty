//! What a runtime cast to a type parameter checks.
//!
//! A type parameter is erased at run time: `x as R` asks whether `x` is an instance of the
//! erasure of `R`, its first written upper bound, as kotlinc's `checkcast` does. The resolver and
//! the KLIB body lowering record it once on the cast; a target only emits the test.

use std::collections::HashSet;

use super::{wk, Ty, TyKind};

impl Ty {
    /// What a cast to `self` checks at run time when `self` is a type parameter: its erasure, the
    /// first upper bound reached through nested parameters, a definitely-non-null form or an
    /// intersection. `None` for any other target (checked as written) and for a parameter that
    /// erases to `Any?`, which has nothing to check. The bound keeps its own nullability, so
    /// `T : CharSequence?` admits `null` although the bare `T` does not say so.
    pub fn cast_erasure(self) -> Option<Ty> {
        let parameter = |ty: Ty| {
            matches!(
                ty.kind(),
                TyKind::TyParam(..) | TyKind::Var(..) | TyKind::DefinitelyNotNull(_)
            )
        };
        if !parameter(self.non_null()) {
            return None;
        }
        let mut current = self.non_null();
        let mut seen = HashSet::new();
        loop {
            match current.kind() {
                TyKind::TyParam(name, bound) | TyKind::Var(name, bound) => {
                    if !seen.insert(name) {
                        return None;
                    }
                    if !parameter(bound.non_null())
                        && !matches!(bound.non_null().kind(), TyKind::Intersection(_))
                    {
                        current = bound;
                        break;
                    }
                    current = bound.non_null();
                }
                TyKind::DefinitelyNotNull(inner) => current = inner.non_null(),
                TyKind::Intersection(parts) => current = *parts.as_slice().first()?,
                _ => break,
            }
        }
        (current.non_null() != Ty::obj_name(wk::any())).then_some(current)
    }
}
