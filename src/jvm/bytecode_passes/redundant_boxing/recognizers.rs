//! The instructions kotlinc's boxing analysis recognizes (`BoxingInterpreter.kt`'s extension
//! functions): boxings, unboxings, the iterator of a progression, and the comparisons of two boxes
//! it can make on the unboxed values.

use super::super::descriptors;
use super::super::opcodes::*;
use super::values::unboxed_primitive;
use crate::jvm::method_node::Insn;

const KCLASS_TO_CLASS: &str = "(Lkotlin/reflect/KClass;)Ljava/lang/Class;";
const CLASS_TO_KCLASS: &str = "(Ljava/lang/Class;)Lkotlin/reflect/KClass;";

/// What the boxing analysis needs to know about the classes a method mentions: the underlying
/// type of a value class, by internal name, as the class's metadata declares it (kotlinc's
/// `unboxedTypeOfInlineClass`).
pub(crate) trait ValueClasses {
    /// The descriptor of the value class's underlying type; `None` when the class is not one.
    fn underlying_type(&self, internal_name: &str) -> Option<String>;
}

fn method(insn: &Insn, opcode: u8) -> Option<(&str, &str, &str)> {
    match insn {
        Insn::Method {
            op,
            owner,
            name,
            desc,
            ..
        } if *op == opcode => Some((owner, name, desc)),
        _ => None,
    }
}

fn is_wrapper(owner: &str) -> bool {
    unboxed_primitive(&descriptors::of_internal_name(owner)).is_some()
}

/// `isBoxing`: a primitive's `valueOf`, `Reflection.getOrCreateKotlinClass`, a value class's
/// `box-impl` or a coroutine `Boxing.boxInt`-style helper.
pub(super) fn is_boxing(insn: &Insn, value_classes: &dyn ValueClasses) -> bool {
    let Some((owner, name, desc)) = method(insn, INVOKESTATIC) else {
        return false;
    };
    is_primitive_boxing(owner, name, desc)
        || is_class_boxing(owner, name, desc)
        || is_value_class_boxing(owner, name, desc, value_classes)
        || is_coroutine_primitive_boxing(owner, name, desc)
}

fn is_primitive_boxing(owner: &str, name: &str, desc: &str) -> bool {
    let Some(primitive) = unboxed_primitive(&descriptors::of_internal_name(owner)) else {
        return false;
    };
    name == "valueOf" && desc == format!("({primitive})L{owner};")
}

fn is_class_boxing(owner: &str, name: &str, desc: &str) -> bool {
    owner == "kotlin/jvm/internal/Reflection"
        && name == "getOrCreateKotlinClass"
        && desc == CLASS_TO_KCLASS
}

/// `isJavaLangClassBoxing` of an instruction.
pub(super) fn is_class_boxing_insn(insn: &Insn) -> bool {
    method(insn, INVOKESTATIC).is_some_and(|(owner, name, desc)| is_class_boxing(owner, name, desc))
}

fn is_value_class_boxing(
    owner: &str,
    name: &str,
    desc: &str,
    value_classes: &dyn ValueClasses,
) -> bool {
    name == "box-impl"
        && value_classes
            .underlying_type(owner)
            .is_some_and(|underlying| desc == format!("({underlying})L{owner};"))
}

fn is_coroutine_primitive_boxing(owner: &str, name: &str, desc: &str) -> bool {
    if owner != "kotlin/coroutines/jvm/internal/Boxing" {
        return false;
    }
    matches!(
        (name, desc),
        ("boxBoolean", "(Z)Ljava/lang/Boolean;")
            | ("boxChar", "(C)Ljava/lang/Character;")
            | ("boxByte", "(B)Ljava/lang/Byte;")
            | ("boxShort", "(S)Ljava/lang/Short;")
            | ("boxInt", "(I)Ljava/lang/Integer;")
            | ("boxFloat", "(F)Ljava/lang/Float;")
            | ("boxLong", "(J)Ljava/lang/Long;")
            | ("boxDouble", "(D)Ljava/lang/Double;")
    )
}

/// `isUnboxing`: a wrapper's (or `Number`'s) `intValue`-style call, `JvmClassMappingKt.getJavaClass`
/// or a value class's `unbox-impl`.
pub(super) fn is_unboxing(insn: &Insn, value_classes: &dyn ValueClasses) -> bool {
    if let Some((owner, name, desc)) = method(insn, INVOKEVIRTUAL) {
        let primitive = (is_wrapper(owner) || owner == "java/lang/Number")
            && matches!(
                (name, desc),
                ("booleanValue", "()Z")
                    | ("charValue", "()C")
                    | ("byteValue", "()B")
                    | ("shortValue", "()S")
                    | ("intValue", "()I")
                    | ("floatValue", "()F")
                    | ("longValue", "()J")
                    | ("doubleValue", "()D")
            );
        let value_class = name == "unbox-impl"
            && value_classes
                .underlying_type(owner)
                .is_some_and(|underlying| desc == format!("(){underlying}"));
        return primitive || value_class;
    }
    is_class_unboxing(insn)
}

/// `isJavaLangClassUnboxing`.
pub(super) fn is_class_unboxing(insn: &Insn) -> bool {
    method(insn, INVOKESTATIC).is_some_and(|(owner, name, desc)| {
        owner == "kotlin/jvm/JvmClassMappingKt" && name == "getJavaClass" && desc == KCLASS_TO_CLASS
    })
}

/// `isIteratorMethodCall`: `Iterable.iterator()` through an interface.
pub(super) fn is_iterator_call(insn: &Insn) -> bool {
    method(insn, INVOKEINTERFACE)
        .is_some_and(|(_, name, desc)| name == "iterator" && desc == "()Ljava/util/Iterator;")
}

/// An interface call of `next` (the analysis only runs when a method has a boxing or one).
pub(super) fn is_interface_next(insn: &Insn) -> bool {
    method(insn, INVOKEINTERFACE)
        .is_some_and(|(_, name, desc)| name == "next" && desc == "()Ljava/lang/Object;")
}

/// `isProgressionClass`, of a value's descriptor.
pub(super) fn is_progression_class(descriptor: &str) -> bool {
    matches!(
        descriptor,
        "Lkotlin/ranges/CharRange;"
            | "Lkotlin/ranges/CharProgression;"
            | "Lkotlin/ranges/IntRange;"
            | "Lkotlin/ranges/IntProgression;"
            | "Lkotlin/ranges/LongRange;"
            | "Lkotlin/ranges/LongProgression;"
    )
}

/// `isAreEqualIntrinsic`: `Intrinsics.areEqual(Object, Object)`.
pub(super) fn is_are_equal(insn: &Insn) -> bool {
    method(insn, INVOKESTATIC).is_some_and(|(owner, name, desc)| {
        owner == "kotlin/jvm/internal/Intrinsics"
            && name == "areEqual"
            && desc == "(Ljava/lang/Object;Ljava/lang/Object;)Z"
    })
}

/// `isJavaLangComparableCompareTo`.
pub(super) fn is_comparable_compare_to(insn: &Insn) -> bool {
    method(insn, INVOKEINTERFACE).is_some_and(|(owner, name, desc)| {
        owner == "java/lang/Comparable" && name == "compareTo" && desc == "(Ljava/lang/Object;)I"
    })
}

/// kotlinc's `getUnboxedType`: the primitive of a wrapper, `Class` of a `KClass`, or a value
/// class's underlying type.
pub(super) fn unboxed_type(boxed: &str, value_classes: &dyn ValueClasses) -> Option<String> {
    if let Some(primitive) = unboxed_primitive(boxed) {
        return Some(primitive.to_string());
    }
    if boxed == "Lkotlin/reflect/KClass;" {
        return Some("Ljava/lang/Class;".to_string());
    }
    value_classes.underlying_type(descriptors::internal_name(boxed))
}
