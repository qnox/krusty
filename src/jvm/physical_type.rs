//! Physical JVM types shared by representation planning and emission.
//!
//! Declaration descriptors and the emitter name the same JVM type. The conversion lives here so
//! descriptor planning does not call back into emission.

use crate::jvm::names::reference_array_element;
use crate::types::Ty;

/// Physical element stored by a Kotlin reference `Array<T>`. Boxing is selected from the semantic
/// scalar before ordinary JVM erasure collapses unsigned values onto signed carriers; otherwise
/// `Array<UInt>` incorrectly becomes `Integer[]` instead of `kotlin.UInt[]`.
fn jvm_reference_array_element(semantic: Ty) -> Ty {
    let nullable = semantic.is_nullable();
    let semantic = semantic.non_null();
    let boxed = if semantic.is_unsigned() {
        // A bare unsigned `Ty` denotes its primitive carrier. Its nullable spelling is the existing
        // JVM-reference marker for the unsigned wrapper (`UInt?` -> `Lkotlin/UInt;`), and prevents
        // `Ty::array` from selecting the specialized `UIntArray`/`[I` representation here.
        Ty::nullable(semantic)
    } else if crate::jvm::names::boxed_primitive_array_element(semantic).is_some() {
        // `Array<UIntArray>` stores the box. Projecting the element through `ir_ty_to_jvm` would
        // turn it into `IntArray` and allocate `int[][]`.
        semantic
    } else {
        reference_array_element(ir_ty_to_jvm(&semantic))
    };
    if nullable {
        Ty::nullable(boxed)
    } else {
        boxed
    }
}

pub fn ir_ty_to_jvm(t: &Ty) -> Ty {
    // A nullable PRIMITIVE is a JVM reference — its boxed wrapper (`Int?` → `java/lang/Integer`, a
    // 1-slot reference), NOT the unboxed scalar. Map it before peeling `?`, so descriptors, slots and
    // stackmap frames all see the reference. A nullable REFERENCE keeps its descriptor (peel below).
    if let Ty::Nullable(inner) | Ty::PlatformNullable(inner) = t {
        if **inner == Ty::Nothing {
            // In a VALUE position this is the null-only bottom type, which also arises when generic
            // inference combines only `null` arguments. Keep its erased top representation here;
            // declaration descriptors use `jvm_declared_ty` and name inferred or explicit
            // `Nothing?` as Void.
            return Ty::obj("kotlin/Any");
        }
        if **inner == Ty::Unit {
            return Ty::obj("kotlin/Unit");
        }
        if inner.is_unsigned() {
            // Unlike a signed primitive wrapper, an unsigned box has the same classifier name as
            // its semantic scalar. Preserve the nullable type itself as the reference-slot marker;
            // returning bare `UInt` here would necessarily mean the unboxed `int` carrier.
            return *t;
        }
        if let Some(boxed) = inner.boxed_ref() {
            // `boxed_ref` already picks the right wrapper — `java/lang/Integer` for `Int?`, the inline-class
            // `kotlin/UInt` for `UInt?` — so do NOT re-map through `ir_ty_to_jvm` (which would erase the
            // unsigned wrapper to `Integer`).
            return boxed;
        }
    }
    // Nullability is otherwise erased at the JVM-type level (a nullable reference keeps its descriptor),
    // so peel the `?` first.
    match t.non_null() {
        Ty::Unit => Ty::Unit,
        Ty::Nothing => Ty::Nothing,
        // `null` has its own JVM verification type. Preserve it through slot lowering so loop and
        // resume frames describe an always-null local as `Null`, not as the unusable `Top` type.
        Ty::Null => Ty::Null,
        // Bare scalar/`String` variants are already JVM types — pass through. (Checked/common-IR types
        // can arrive either as these variants or as their `Obj("kotlin/…")` spelling; both must map here.)
        Ty::Int => Ty::Int,
        Ty::Long => Ty::Long,
        Ty::Short => Ty::Short,
        Ty::Byte => Ty::Byte,
        Ty::Boolean => Ty::Boolean,
        Ty::Char => Ty::Char,
        Ty::Double => Ty::Double,
        Ty::Float => Ty::Float,
        Ty::String => Ty::String,
        // Unsigned scalars are inline classes over the signed primitive; unboxed they ARE that primitive
        // (`UInt` = `int`, `ULong` = `long`) — same JVM slots and `istore`/`iload`/arithmetic. Unsigned
        // semantics live in the intrinsic calls (`Integer.compareUnsigned`, …) common lowering inserted.
        Ty::UByte => Ty::Byte,
        Ty::UShort => Ty::Short,
        Ty::UInt => Ty::Int,
        Ty::ULong => Ty::Long,
        Ty::Obj(fq_name, type_args) => {
            // Arrays are regular class types the JVM backend lowers to JVM array types here. Every
            // primitive specialized array, signed and unsigned alike, goes through the one operation
            // that identifies them and the one that decides an element's width — see
            // `jvm::array_representation`.
            if let Some(carrier) = crate::jvm::array_representation::prim_array_carrier(fq_name) {
                return carrier;
            }
            match () {
                _ if fq_name.matches("kotlin/Int") => Ty::Int,
                _ if fq_name.matches("kotlin/Long") => Ty::Long,
                _ if fq_name.matches("kotlin/Short") => Ty::Short,
                _ if fq_name.matches("kotlin/Byte") => Ty::Byte,
                _ if fq_name.matches("kotlin/Boolean") => Ty::Boolean,
                _ if fq_name.matches("kotlin/Char") => Ty::Char,
                _ if fq_name.matches("kotlin/Double") => Ty::Double,
                _ if fq_name.matches("kotlin/Float") => Ty::Float,
                _ if fq_name.matches("kotlin/String") => Ty::String,
                // A `kotlin/Array<T>` is a JVM reference array: a primitive element `T` is BOXED
                // (`Array<Int>` = `[Ljava/lang/Integer;`, distinct from the unboxed `IntArray` = `[I`).
                _ if fq_name.matches("kotlin/Array") => Ty::array(
                    type_args
                        .first()
                        .map(|e| {
                            // A projection is valid here as the ARRAY classifier's type argument, even
                            // though it is never a value type of its own. Erase it at this boundary:
                            // `out X` has the readable element `X`; `in X` can only be read as `Any`.
                            let semantic = match e.non_null() {
                                Ty::OutProjection(inner) | Ty::StarProjection(inner) => *inner,
                                Ty::InProjection(_) => Ty::obj("kotlin/Any"),
                                _ => *e,
                            };
                            let boxed = jvm_reference_array_element(semantic);
                            // Keep a NULLABLE element's `?`: `Array<Int?>` = `Integer[]` whose `get` yields the
                            // BOXED element (it can be `null`), UNLIKE `Array<Int>` whose `get` unboxes.
                            // `boxed_prim_of` returns `None` for a `Nullable(..)`, so the emitter's `Array.get`
                            // keeps it boxed and `.set` skips the extra box — matching the value the front end
                            // supplies (boxed for a nullable element, unboxed for a non-null one).
                            if e.is_nullable() {
                                Ty::nullable(boxed)
                            } else {
                                boxed
                            }
                        })
                        .unwrap_or(Ty::obj("java/lang/Object")),
                ),
                _ => Ty::obj_name(crate::jvm::jvm_class_map::to_jvm_classfile_type_name(
                    fq_name,
                )),
            }
        }
        // The JVM representation of a function type is `kotlin/jvm/functions/FunctionN`. A `suspend`
        // function type carries a trailing `Continuation` parameter, so its arity is one greater.
        Ty::Fun(s) => Ty::obj(&crate::jvm::names::function_interface_internal_name(
            s.params.len() + usize::from(s.suspend),
        )),
        // JVM erasure of a type parameter: collapse `T` to its declared upper bound (which itself
        // erases to `java/lang/Object` for an `Any` bound). This is the ONE place `T` becomes a
        // concrete JVM type.
        // A nullable occurrence keeps its `?` on the bound, as kotlinc's type mapper does: `T?` over
        // `T : Int` is `Integer`, never `int`.
        Ty::TyParam(_, bound) if t.is_nullable() => ir_ty_to_jvm(&Ty::nullable(*bound)),
        Ty::TyParam(_, bound) => ir_ty_to_jvm(bound),
        _ => Ty::Error,
    }
}
