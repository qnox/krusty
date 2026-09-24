//! JVM scalar representation transitions.
//!
//! Checked IR supplies semantic types. This module is the one emission boundary that chooses JVM
//! wrappers, primitive accessors, numeric conversions, and unsigned value-class adapters.

use super::{ir_ty_to_jvm, slot_words, type_descriptor, ClassWriter, CodeBuilder, Ty, TypeName};

/// Box a primitive on the stack to its wrapper. Signed primitives use `valueOf`; unsigned scalars
/// use their inline-class `box-impl`.
pub(super) fn box_prim_free(cw: &mut ClassWriter, code: &mut CodeBuilder, ty: Ty) {
    let Some((owner, method, descriptor)) = scalar_box_accessor(ty) else {
        return;
    };
    let method = cw.methodref(owner, method, descriptor);
    code.invokestatic(method, slot_words(ty) as i32, 1);
}

fn scalar_box_accessor(ty: Ty) -> Option<(&'static str, &'static str, &'static str)> {
    Some(match ty {
        Ty::Int => ("java/lang/Integer", "valueOf", "(I)Ljava/lang/Integer;"),
        Ty::Long => ("java/lang/Long", "valueOf", "(J)Ljava/lang/Long;"),
        Ty::Double => ("java/lang/Double", "valueOf", "(D)Ljava/lang/Double;"),
        Ty::Float => ("java/lang/Float", "valueOf", "(F)Ljava/lang/Float;"),
        Ty::Boolean => ("java/lang/Boolean", "valueOf", "(Z)Ljava/lang/Boolean;"),
        Ty::Char => ("java/lang/Character", "valueOf", "(C)Ljava/lang/Character;"),
        Ty::Byte => ("java/lang/Byte", "valueOf", "(B)Ljava/lang/Byte;"),
        Ty::Short => ("java/lang/Short", "valueOf", "(S)Ljava/lang/Short;"),
        Ty::UByte => ("kotlin/UByte", "box-impl", "(B)Lkotlin/UByte;"),
        Ty::UShort => ("kotlin/UShort", "box-impl", "(S)Lkotlin/UShort;"),
        Ty::UInt => ("kotlin/UInt", "box-impl", "(I)Lkotlin/UInt;"),
        Ty::ULong => ("kotlin/ULong", "box-impl", "(J)Lkotlin/ULong;"),
        _ => return None,
    })
}

/// Select the scalar whose adapter owns a reference boundary before reducing it to its carrier.
pub(super) fn semantic_scalar_adapter(semantic: Ty, carrier: Ty) -> Ty {
    fn concrete_scalar(mut ty: Ty) -> Option<Ty> {
        loop {
            match ty.non_null().canonical_semantic() {
                Ty::TyParam(_, bound) => ty = *bound,
                scalar @ (Ty::Int
                | Ty::Long
                | Ty::Double
                | Ty::Float
                | Ty::Boolean
                | Ty::Char
                | Ty::Byte
                | Ty::Short
                | Ty::UByte
                | Ty::UShort
                | Ty::UInt
                | Ty::ULong) => return Some(scalar),
                _ => return None,
            }
        }
    }
    if !carrier.is_jvm_scalar() {
        return carrier;
    }
    concrete_scalar(semantic).unwrap_or(carrier)
}

/// JVM implementation owner and carrier for one built-in unsigned value class.
pub(super) fn native_unsigned_impl_target(semantic: Ty) -> Option<(TypeName, Ty)> {
    let semantic = semantic.non_null().canonical_semantic();
    semantic.is_unsigned().then(|| {
        (
            semantic
                .kotlin_class_internal()
                .expect("an unsigned scalar has a Kotlin classifier"),
            ir_ty_to_jvm(&semantic),
        )
    })
}

#[derive(Clone, Copy)]
struct ScalarUnboxAccessor {
    owner: &'static str,
    method: &'static str,
    descriptor: &'static str,
}

fn scalar_unbox_accessor(ty: Ty) -> Option<ScalarUnboxAccessor> {
    let (owner, method, descriptor) = match ty {
        Ty::Int => ("java/lang/Integer", "intValue", "()I"),
        Ty::Long => ("java/lang/Long", "longValue", "()J"),
        Ty::Double => ("java/lang/Double", "doubleValue", "()D"),
        Ty::Float => ("java/lang/Float", "floatValue", "()F"),
        Ty::Boolean => ("java/lang/Boolean", "booleanValue", "()Z"),
        Ty::Char => ("java/lang/Character", "charValue", "()C"),
        Ty::Byte => ("java/lang/Byte", "byteValue", "()B"),
        Ty::Short => ("java/lang/Short", "shortValue", "()S"),
        Ty::UByte => ("kotlin/UByte", "unbox-impl", "()B"),
        Ty::UShort => ("kotlin/UShort", "unbox-impl", "()S"),
        Ty::UInt => ("kotlin/UInt", "unbox-impl", "()I"),
        Ty::ULong => ("kotlin/ULong", "unbox-impl", "()J"),
        _ => return None,
    };
    Some(ScalarUnboxAccessor {
        owner,
        method,
        descriptor,
    })
}

fn required_unbox_accessor(ty: Ty) -> ScalarUnboxAccessor {
    scalar_unbox_accessor(ty).unwrap_or_else(|| {
        panic!("JVM scalar unboxing requires an exact scalar adapter, got {ty:?}")
    })
}

/// Unbox a reference with static JVM type `from` to scalar `target`.
pub(super) fn unbox_prim_from(cw: &mut ClassWriter, code: &mut CodeBuilder, from: Ty, target: Ty) {
    unbox_prim_from_descriptor(cw, code, &type_descriptor(from), target);
}

/// Descriptor form used when the physical source type already comes from a JVM signature.
pub(super) fn unbox_prim_from_descriptor(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    from: &str,
    target: Ty,
) {
    let from_internal = from
        .strip_prefix('L')
        .and_then(|descriptor| descriptor.strip_suffix(';'))
        .unwrap_or("java/lang/Object");
    crate::trace_compiler!(
        "value_classes",
        "emit scalar unbox from={from_internal} adapter={target:?}"
    );
    let target_accessor = required_unbox_accessor(target);
    if target.is_unsigned() {
        if from_internal != target_accessor.owner {
            let class = cw.class_ref(target_accessor.owner);
            code.checkcast(class);
        }
        let method = cw.methodref(
            target_accessor.owner,
            target_accessor.method,
            target_accessor.descriptor,
        );
        code.invokevirtual(method, 0, slot_words(target) as i32);
        return;
    }
    if let Some(source) = wrapper_owner_primitive(from_internal) {
        let accessor = required_unbox_accessor(source);
        let method = cw.methodref(from_internal, accessor.method, accessor.descriptor);
        code.invokevirtual(method, 0, slot_words(source) as i32);
        emit_num_conv(source, target, code);
        return;
    }
    match target {
        Ty::Boolean => unbox_prim(cw, code, target),
        Ty::Char if from_internal != "java/lang/Number" => unbox_prim(cw, code, target),
        Ty::Char => {
            let method = cw.methodref("java/lang/Number", "intValue", "()I");
            code.invokevirtual(method, 0, 1);
            emit_num_conv(Ty::Int, Ty::Char, code);
        }
        Ty::Int | Ty::Long | Ty::Double | Ty::Float | Ty::Byte | Ty::Short => {
            if from_internal != "java/lang/Number" {
                let number = cw.class_ref("java/lang/Number");
                code.checkcast(number);
            }
            let method = cw.methodref(
                "java/lang/Number",
                target_accessor.method,
                target_accessor.descriptor,
            );
            code.invokevirtual(method, 0, slot_words(target) as i32);
        }
        _ => unreachable!("required_unbox_accessor accepted a non-scalar target"),
    }
}

/// Unbox through the target scalar's own wrapper.
pub(super) fn unbox_prim(cw: &mut ClassWriter, code: &mut CodeBuilder, target: Ty) {
    crate::trace_compiler!("value_classes", "emit scalar unbox adapter={target:?}");
    let accessor = required_unbox_accessor(target);
    let class = cw.class_ref(accessor.owner);
    code.checkcast(class);
    let method = cw.methodref(accessor.owner, accessor.method, accessor.descriptor);
    code.invokevirtual(method, 0, slot_words(target) as i32);
}

/// Convert the numeric primitive on top of the stack from `from` to `to`.
pub(super) fn emit_num_conv(from: Ty, to: Ty, code: &mut CodeBuilder) {
    if from == to {
        return;
    }
    let wide = |ty: Ty| match ty {
        Ty::Byte | Ty::Short | Ty::Char | Ty::Int => Ty::Int,
        other => other,
    };
    match (wide(from), wide(to)) {
        (Ty::Int, Ty::Long) => code.i2l(),
        (Ty::Int, Ty::Float) => code.i2f(),
        (Ty::Int, Ty::Double) => code.i2d(),
        (Ty::Long, Ty::Int) => code.l2i(),
        (Ty::Long, Ty::Float) => code.l2f(),
        (Ty::Long, Ty::Double) => code.l2d(),
        (Ty::Float, Ty::Int) => code.f2i(),
        (Ty::Float, Ty::Long) => code.f2l(),
        (Ty::Float, Ty::Double) => code.f2d(),
        (Ty::Double, Ty::Int) => code.d2i(),
        (Ty::Double, Ty::Long) => code.d2l(),
        (Ty::Double, Ty::Float) => code.d2f(),
        _ => {}
    }
    match to {
        Ty::Byte => code.i2b(),
        Ty::Short => code.i2s(),
        Ty::Char => code.i2c(),
        _ => {}
    }
}

pub(super) fn wrapper_owner_primitive(owner: &str) -> Option<Ty> {
    Some(match owner {
        "java/lang/Integer" | "kotlin/Int" => Ty::Int,
        "java/lang/Long" | "kotlin/Long" => Ty::Long,
        "java/lang/Double" | "kotlin/Double" => Ty::Double,
        "java/lang/Float" | "kotlin/Float" => Ty::Float,
        "java/lang/Boolean" | "kotlin/Boolean" => Ty::Boolean,
        "java/lang/Character" | "kotlin/Char" => Ty::Char,
        "java/lang/Byte" | "kotlin/Byte" => Ty::Byte,
        "java/lang/Short" | "kotlin/Short" => Ty::Short,
        _ => return None,
    })
}
