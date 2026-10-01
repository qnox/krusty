//! Physical JVM types shared by representation planning and emission.
//!
//! Declaration descriptors and the emitter name the same JVM type. The conversion lives here so
//! descriptor planning does not call back into emission.

use crate::jvm::names::reference_array_element;
use crate::types::{Ty, TypeName};

/// Signed scalar and `String` classifiers rewritten from an object type at the JVM physical-type
/// boundary. Unsigned classifiers, `Unit`, and `Nothing` stay with the other representation arms.
fn jvm_builtin_scalar(name: TypeName) -> Option<Ty> {
    match crate::types::builtin_semantic(name) {
        Some(ty) if !ty.is_unsigned() && ty != Ty::Unit && ty != Ty::Nothing => Some(ty),
        _ => None,
    }
}

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

/// Classifier named by a class descriptor. Every `L...;` is this path: no unsigned-name branch
/// and no nullability. [`FieldSlot`] records that the descriptor is a reference slot, because
/// `kotlin/UInt` is also the semantic scalar and `Ty` cannot say both.
pub(super) fn class_descriptor_ty(descriptor: &str) -> Ty {
    let internal = descriptor
        .strip_prefix('L')
        .and_then(|name| name.strip_suffix(';'))
        .unwrap_or(descriptor);
    Ty::obj_name(crate::types::type_name(internal))
}

/// Physical category of one JVM field descriptor.
///
/// `reference` is the descriptor's own slot. It is true for every class and array descriptor,
/// including `Lkotlin/UInt;`, and it is not semantic nullability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FieldSlot {
    pub(crate) ty: Ty,
    pub(crate) reference: bool,
}

impl FieldSlot {
    pub(crate) fn words(self) -> i32 {
        if self.reference {
            1
        } else {
            match self.ty {
                Ty::Long | Ty::Double => 2,
                Ty::Unit => 0,
                _ => 1,
            }
        }
    }
}

pub(crate) fn method_return_slot(descriptor: &str) -> FieldSlot {
    let ret = descriptor.rsplit(')').next().unwrap_or("V");
    field_slot(ret)
}

pub(crate) fn field_slot(descriptor: &str) -> FieldSlot {
    match descriptor.as_bytes().first() {
        Some(b'I') => FieldSlot {
            ty: Ty::Int,
            reference: false,
        },
        Some(b'J') => FieldSlot {
            ty: Ty::Long,
            reference: false,
        },
        Some(b'Z') => FieldSlot {
            ty: Ty::Boolean,
            reference: false,
        },
        Some(b'B') => FieldSlot {
            ty: Ty::Byte,
            reference: false,
        },
        Some(b'C') => FieldSlot {
            ty: Ty::Char,
            reference: false,
        },
        Some(b'S') => FieldSlot {
            ty: Ty::Short,
            reference: false,
        },
        Some(b'F') => FieldSlot {
            ty: Ty::Float,
            reference: false,
        },
        Some(b'D') => FieldSlot {
            ty: Ty::Double,
            reference: false,
        },
        Some(b'V') => FieldSlot {
            ty: Ty::Unit,
            reference: false,
        },
        Some(b'L') => FieldSlot {
            ty: class_descriptor_ty(descriptor),
            reference: true,
        },
        Some(b'[') => {
            let element = field_slot(&descriptor[1..]);
            // A class element stays a reference array. `Ty::array` would see the scalar
            // classifier `kotlin/UInt` and select the specialized `UIntArray` (`[I`).
            let ty = if descriptor_element_is_reference(&descriptor[1..]) {
                reference_array_slot(element.ty)
            } else {
                Ty::array(element.ty)
            };
            FieldSlot {
                ty,
                reference: true,
            }
        }
        _ => FieldSlot {
            ty: Ty::Error,
            reference: false,
        },
    }
}

/// Operand type for a descriptor slot.
///
/// Every class or reference-array slot is paired with the checked type when that type is itself
/// a non-scalar reference and not a specialized primitive array. A missing or unusable checked
/// type keeps the parsed slot when that slot is already such a reference, and otherwise a plain
/// object reference. A primitive-array descriptor (`[I`) keeps the specialized array it names.
/// Neither path invents nullability from the descriptor spelling.
pub(crate) fn operand_slot_ty(descriptor: &str, checked: Option<Ty>) -> Ty {
    let slot = field_slot(descriptor);
    if !slot.reference || primitive_array_descriptor(descriptor) {
        return slot.ty;
    }
    checked
        .map(|ty| ir_ty_to_jvm(&crate::types::stored_value_ty(ty)))
        .filter(|ty| usable_reference_operand(*ty))
        .unwrap_or_else(|| {
            if usable_reference_operand(slot.ty) {
                slot.ty
            } else {
                Ty::obj("java/lang/Object")
            }
        })
}

/// `[I` and `[[I` name specialized primitive arrays. `[L…;` does not.
fn primitive_array_descriptor(descriptor: &str) -> bool {
    let element = descriptor.trim_start_matches('[');
    descriptor.len() > element.len()
        && element.len() == 1
        && matches!(
            element.as_bytes().first(),
            Some(b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z')
        )
}

fn descriptor_element_is_reference(descriptor: &str) -> bool {
    matches!(descriptor.as_bytes().first(), Some(b'L' | b'['))
        && !primitive_array_descriptor(descriptor)
}

/// A class-array slot. A scalar classifier cannot be the element: `Ty::array` would turn it into
/// the specialized primitive array whose descriptor is the carrier, not `[Lkotlin/UInt;`.
fn reference_array_slot(element: Ty) -> Ty {
    let element = if element.is_jvm_scalar() || specialized_primitive_array(element) {
        Ty::obj("java/lang/Object")
    } else {
        element
    };
    Ty::obj_args_name(crate::types::wk::array(), &[element])
}

fn usable_reference_operand(ty: Ty) -> bool {
    ty.is_reference() && !ty.is_jvm_scalar() && !specialized_primitive_array(ty)
}

fn specialized_primitive_array(ty: Ty) -> bool {
    ty.obj_internal()
        .is_some_and(|name| crate::types::prim_array_element(name).is_some())
}

pub(crate) fn constructor_operand_tys(descriptor: &str, checked: Option<&[Ty]>) -> Option<Vec<Ty>> {
    let (params, _) = crate::jvm::names::parse_method_descriptor(descriptor)?;
    Some(
        params
            .into_iter()
            .enumerate()
            .map(|(index, param)| {
                operand_slot_ty(param, checked.and_then(|types| types.get(index)).copied())
            })
            .collect(),
    )
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
            if let Some(scalar) = jvm_builtin_scalar(fq_name) {
                return scalar;
            }
            // A `kotlin/Array<T>` is a JVM reference array: a primitive element `T` is BOXED
            // (`Array<Int>` = `[Ljava/lang/Integer;`, distinct from the unboxed `IntArray` = `[I`).
            if fq_name == crate::types::wk::array() {
                return Ty::array(
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
                );
            }
            Ty::obj_name(crate::jvm::jvm_class_map::to_jvm_classfile_type_name(
                fq_name,
            ))
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

#[cfg(test)]
mod tests {
    use super::jvm_builtin_scalar;
    use crate::types::{type_name, Ty};

    #[test]
    fn every_class_descriptor_is_a_reference_slot_without_semantic_nullability() {
        for descriptor in [
            "Lkotlin/UByte;",
            "Lkotlin/UShort;",
            "Lkotlin/UInt;",
            "Lkotlin/ULong;",
            "Lexample/Point;",
            "Ljava/lang/String;",
        ] {
            let slot = super::field_slot(descriptor);
            assert!(slot.reference, "{descriptor}");
            assert!(!slot.ty.is_nullable(), "{descriptor}");
            assert_eq!(slot.words(), 1, "{descriptor}");
        }
        let point = super::field_slot("Lexample/Point;");
        assert_eq!(point.ty, Ty::obj("example/Point"));
        assert!(!point.ty.is_jvm_scalar());

        let unsigned = super::field_slot("Lkotlin/UInt;");
        assert!(
            unsigned.ty.is_jvm_scalar(),
            "the classifier is still the scalar"
        );
        let checked = Ty::nullable(Ty::UInt);
        let operand = super::operand_slot_ty("Lkotlin/UInt;", Some(checked));
        assert!(operand.is_reference() && !operand.is_jvm_scalar());
        assert_eq!(operand.is_nullable(), checked.is_nullable());
        let bare = super::operand_slot_ty("Lkotlin/UInt;", None);
        assert!(bare.is_reference());
        assert!(!bare.is_nullable());
        assert!(!bare.is_jvm_scalar());

        let carrier = super::field_slot("I");
        assert!(!carrier.reference);
        assert_eq!(carrier.ty, Ty::Int);
        assert!(!super::operand_slot_ty("Ljava/lang/String;", None).is_nullable());
    }

    #[test]
    fn a_boxed_unsigned_array_is_not_the_primitive_array() {
        let boxed = super::field_slot("[Lkotlin/UInt;");
        let primitive = super::field_slot("[I");
        assert!(boxed.reference && primitive.reference);
        assert!(boxed.ty.is_reference_array(), "{:?}", boxed.ty);
        assert!(!super::specialized_primitive_array(boxed.ty));
        assert!(super::specialized_primitive_array(primitive.ty));
        assert_eq!(crate::jvm::names::type_descriptor(primitive.ty), "[I");
        assert_ne!(
            crate::jvm::names::type_descriptor(boxed.ty),
            "[I",
            "a class array must not collapse to the unsigned carrier array"
        );

        let checked = Ty::obj_args_name(crate::types::wk::array(), &[Ty::UInt]);
        let operand = super::operand_slot_ty("[Lkotlin/UInt;", Some(checked));
        assert!(operand.is_reference_array());
        assert!(!super::specialized_primitive_array(operand));
        assert_eq!(
            crate::jvm::names::type_descriptor(operand),
            "[Lkotlin/UInt;"
        );
        assert_eq!(
            crate::jvm::names::type_descriptor(super::operand_slot_ty("[I", Some(checked))),
            "[I"
        );
        let bare = super::operand_slot_ty("[Lkotlin/UInt;", None);
        assert!(bare.is_reference_array(), "{:?}", bare);
        assert!(!super::specialized_primitive_array(bare));
        assert_eq!(
            crate::jvm::names::type_descriptor(bare),
            "[Ljava/lang/Object;"
        );
        assert_eq!(
            crate::jvm::names::type_descriptor(super::field_slot("[Ljava/lang/String;").ty),
            "[Ljava/lang/String;"
        );
        assert_eq!(
            crate::jvm::names::type_descriptor(super::field_slot("[[I").ty),
            "[[I"
        );

        let constructor = super::constructor_operand_tys("([Lkotlin/UInt;)V", Some(&[checked]))
            .expect("constructor descriptor");
        assert_eq!(constructor.len(), 1);
        assert_eq!(
            crate::jvm::names::type_descriptor(constructor[0]),
            "[Lkotlin/UInt;"
        );
    }

    #[test]
    fn signed_scalars_and_string_are_the_physical_subset() {
        assert_eq!(jvm_builtin_scalar(type_name("kotlin/Int")), Some(Ty::Int));
        assert_eq!(
            jvm_builtin_scalar(type_name("kotlin/String")),
            Some(Ty::String)
        );
        assert_eq!(jvm_builtin_scalar(type_name("kotlin/UInt")), None);
        assert_eq!(jvm_builtin_scalar(type_name("kotlin/Unit")), None);
        assert_eq!(jvm_builtin_scalar(type_name("kotlin/Nothing")), None);
        assert_eq!(jvm_builtin_scalar(type_name("kotlin/Array")), None);
    }
}
