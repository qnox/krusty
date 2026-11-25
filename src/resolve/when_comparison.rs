//! Whether a `when` subject and a condition value can be compared with Kotlin equality.

use super::*;
use crate::types::TyKind;

impl Checker<'_> {
    /// Are two reference types comparable as `when`-subject value arms? A `when (s) { A -> … }` over a
    /// sealed subject `s: S` matches the *object* `A` (a subtype of `S`) by `==` — valid in Kotlin
    /// whenever one operand's type is a subtype of the other (the comparison can be non-trivially true).
    /// A primitive or `String` operand takes part as its own Kotlin classifier (`kotlin/Int` declares
    /// `Number` and `Comparable<Int>`): `when (x: Any) { 1 -> … }` compares an `Int` with the `Any`
    /// subject, and the comparison can be non-trivially true for exactly the same subtype reason. How
    /// a target boxes the value is its own representation, never a semantic supertype.
    pub(super) fn when_objs_comparable(&self, st: Ty, ct: Ty) -> bool {
        // `Unit` is not a reference type in the checked model, but `Unit -> …` compares the
        // `kotlin/Unit` object with the subject like any other object arm.
        let as_classifier = |ty: Ty| match ty.kind() {
            TyKind::Unit => Ty::obj_name(crate::types::wk::unit()),
            _ => ty,
        };
        match (as_classifier(st).kind(), as_classifier(ct).kind()) {
            (TyKind::Obj(a, _), TyKind::Obj(b, _)) => {
                self.obj_name_is_subtype(a, b) || self.obj_name_is_subtype(b, a)
            }
            _ => false,
        }
    }

    /// Whether subject and condition values can be compared with Kotlin equality.
    pub(super) fn when_values_comparable(&self, subject: Ty, condition: Ty) -> bool {
        let subject = subject.non_null();
        let condition = condition.non_null();
        subject == condition
            || Ty::promote(subject, condition).is_some()
            || self.when_objs_comparable(subject, condition)
    }
}
