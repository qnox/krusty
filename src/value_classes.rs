//! How a `@JvmInline value class` is REPRESENTED, for every backend.
//!
//! Kotlin's defining promise about a value class is that it is erased at run time: `Meters(21)` IS
//! the `21`, with no wrapper object, no header and nothing for a collector to trace. That promise
//! is about the language, not about a target — so the rule that decides it lives here rather than
//! inside one backend, and both the JVM pass and the native code generator read it from one place.
//!
//! What is NOT here is everything a particular target does with the answer: the JVM's name mangling
//! and its `box-impl`/`unbox-impl` methods, its descriptors and bridges, the native generator's
//! carrier selection. Those differ per target and belong to their own backends. What is shared is
//! the semantic question underneath all of them — given a type, what does it erase to, and where
//! must a box appear anyway.
//!
//! The hard parts are the ones a backend must not answer for itself, because answering twice is how
//! two authorities drift apart:
//!
//! * **Nested chains.** A value class over a value class erases through, so `erase` recurses rather
//!   than reading one level.
//! * **Nullability.** `X?` stays UNBOXED when the underlying is a non-null REFERENCE, because that
//!   reference carries `null` itself. Over a machine scalar it cannot, and over a NULLABLE
//!   reference it must not — `X(null)` and a `null` `X?` would otherwise be indistinguishable.

use crate::types::{Ty, TypeName};
use std::collections::HashMap;

/// Value-class internal name -> the type it erases to, before recursive erasure.
///
/// Built by a backend from the source declarations in an `IrFile` and the classpath value classes
/// it references; the rules below read it and never build it, so one map serves every consumer.
pub(crate) type Erasure = HashMap<TypeName, Ty>;

pub(crate) fn erase(t: &Ty, under: &Erasure) -> Ty {
    if let Some(fq_name) = t.non_null().obj_internal() {
        let nullable = t.is_nullable();
        if let Some(u) = under.get(&fq_name) {
            // A non-null `X` always erases to its underlying. A nullable `X?` erases ONLY when it is NOT
            // boxed (`nullable_is_boxed` is the single source of truth — over a non-null reference that
            // carries `null` itself); otherwise it stays the boxed `X` so `X(null)` ≠ `null`. Delegating
            // keeps erasure consistent with the box/unbox analysis for arbitrarily nested chains.
            if !nullable || !nullable_is_boxed(fq_name, under) {
                return erase(u, under);
            }
        }
    }
    *t
}

/// Whether a NULLABLE value class `X?` is represented BOXED. Only true when its underlying erases to a
/// primitive (a primitive can't carry null, so `X?` keeps the boxed `X`). Over a reference underlying,
/// `X?` erases to that underlying reference — represented unboxed, exactly like a non-null `X`.
pub(crate) fn nullable_is_boxed(x: TypeName, under: &Erasure) -> bool {
    // `X?` stays UNBOXED (its underlying reference carries null) only when the underlying is a NON-NULL
    // reference. Over a primitive (can't hold null) OR a NULLABLE reference (where `X(null)` and a `null`
    // `X?` would otherwise be indistinguishable), `X?` is the boxed `X`.
    under
        .get(&x)
        .map(|u| !is_ref(&erase(u, under)) || underlying_null_capable(u, under))
        .unwrap_or(false)
}

/// Whether a value class's unboxed representation can hold `null` — true when ANY level of the nested
/// underlying chain is declared nullable (`X(val v: Int?)`; `ZN(val z: Z1?)` → `ZN2(val z: ZN)` null-capable
/// through `Z1?`). `erase` collapses a nullable-over-non-null-reference to a non-null underlying, so this
/// walks the UNERASED chain to see the `?` erasure drops.
pub(crate) fn underlying_null_capable(t: &Ty, under: &Erasure) -> bool {
    if t.is_nullable() {
        return true;
    }
    match t.obj_internal() {
        Some(fq_name) => under
            .get(&fq_name)
            .is_some_and(|u| underlying_null_capable(u, under)),
        None => false,
    }
}

/// Whether the erased type occupies a JVM *reference* slot. A non-null Kotlin primitive class
/// (`kotlin/Int`, `kotlin/Boolean`, …) emits as a JVM primitive (`I`, `Z`, …), so it is NOT a
/// reference; its NULLABLE form is the boxed wrapper (`Integer`), which is. Everything else that is a
/// `Class` is a reference.
pub(crate) fn is_ref(t: &Ty) -> bool {
    if t.is_nullable() {
        return true;
    }
    // A Kotlin type parameter always occupies an erased JVM reference slot, even when its upper
    // bound names a primitive-like Kotlin class. Treating `T` as non-reference loses the boxing
    // boundary in `Holder<T>(value: T)` and stores an unboxed value-class carrier as `Integer`
    // instead of the value class's boxed wrapper.
    if matches!(t.non_null(), Ty::TyParam(..)) {
        return true;
    }
    // A JVM scalar (`Int`/`Long`/… AND the unsigned `UInt`/`ULong`, which are unboxed primitives) is NOT a
    // reference. Check this FIRST — `kotlin_class_internal(UInt)` is "kotlin/UInt" but `unboxed_primitive`
    // only knows the signed wrappers, so the descriptor check below would misclassify it as a reference.
    if t.is_jvm_scalar() {
        return false;
    }
    // A FUNCTION type realizes as a `FunctionN` object and an array as its array class — both are
    // references with no `kotlin_class_internal`, and the `None => false` fallback below silently
    // stripped their `checkNotNullParameter` guards (kotlinc guards a `block: () -> Unit` like any
    // other non-null reference parameter).
    if matches!(t, Ty::Fun(_)) || t.is_array() {
        return true;
    }
    // `kotlin_class_internal` (not `obj_internal`): a bare `Ty::String` variant is a REFERENCE but has no
    // `obj_internal()` — treating it as a non-reference makes `nullable_is_boxed` think a `String`-backed
    // value class is primitive-like (`Str?` wrongly boxed instead of unboxed to `String?`).
    match t.kotlin_class_internal() {
        Some(fq_name) => Ty::obj(&fq_name.render()).unboxed_primitive().is_none(),
        None => false,
    }
}
