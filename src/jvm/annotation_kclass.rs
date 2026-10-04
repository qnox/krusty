//! JVM storage of a Kotlin annotation member whose semantic type is `KClass`.
//!
//! An annotation interface method returns `java.lang.Class`, and the synthetic implementation
//! stores that class. The constructor still receives the `KClass` the call site built, converts it
//! with `JvmClassMappingKt.getJavaClass`, and `equals` rebuilds a `KClass` before comparing.
//! `hashCode` hashes the stored `Class` through `Object.hashCode`: `Class` declares none of its
//! own, and `Int::class` / `Integer::class` are equal as `KClass` values while their stored
//! classes (`int` and `java.lang.Integer`) hash differently.

use crate::backend::BackendClassifierSource;
use crate::ir::IrFile;
use crate::jvm::classfile::{ClassWriter, CodeBuilder};
use crate::types::{type_name, Ty, TypeName};

fn kclass_classifier() -> TypeName {
    type_name("kotlin/reflect/KClass")
}

fn java_class_classifier() -> TypeName {
    type_name("java/lang/Class")
}

/// The non-null classifier is `kotlin.reflect.KClass`, whatever type argument it carries.
pub(crate) fn is_annotation_kclass(ty: Ty) -> bool {
    ty.non_null().obj_internal() == Some(kclass_classifier())
}

/// `Array<KClass<…>>`. Nested arrays are not annotation `KClass` arrays: kotlinc maps only the
/// outer array of `KClass`.
pub(crate) fn is_annotation_kclass_array(ty: Ty) -> bool {
    let ty = ty.non_null();
    ty.is_reference_array() && ty.array_elem().is_some_and(is_annotation_kclass)
}

/// The JVM type an annotation member of `ty` is stored and returned as.
pub(crate) fn annotation_member_jvm_type(ty: Ty) -> Ty {
    if is_annotation_kclass(ty) {
        Ty::obj("java/lang/Class")
    } else if is_annotation_kclass_array(ty) {
        Ty::array(Ty::obj("java/lang/Class"))
    } else {
        ty
    }
}

/// JVM result of a property read planned without the declaring class file. Annotation members
/// declared `KClass` use `java.lang.Class`; every other property keeps its semantic type.
pub(crate) fn annotation_member_read_type(
    classifiers: &dyn BackendClassifierSource,
    owner: TypeName,
    ty: Ty,
) -> Ty {
    if classifiers
        .classifier(owner)
        .is_some_and(|classifier| classifier.is_annotation())
    {
        annotation_member_jvm_type(ty)
    } else {
        ty
    }
}

/// The single type argument of a `KClass` member. A raw or star `KClass` is `*`.
pub(crate) enum KClassArgument {
    Star,
    Projected(Ty),
}

pub(crate) fn kclass_argument(ty: Ty) -> Option<KClassArgument> {
    if !is_annotation_kclass(ty) {
        return None;
    }
    match ty.non_null().type_args().first().copied() {
        None | Some(Ty::StarProjection(_)) => Some(KClassArgument::Star),
        Some(argument) => Some(KClassArgument::Projected(argument)),
    }
}

/// `KClass.java` — the value on the stack is a `KClass`, the result is its `java.lang.Class`.
pub(crate) fn emit_kclass_to_class(cw: &mut ClassWriter, code: &mut CodeBuilder) {
    let method = cw.methodref(
        "kotlin/jvm/JvmClassMappingKt",
        "getJavaClass",
        "(Lkotlin/reflect/KClass;)Ljava/lang/Class;",
    );
    code.invokestatic(method, 1, 1);
}

fn is_java_class(ty: Ty) -> bool {
    ty.non_null().obj_internal() == Some(java_class_classifier())
}

fn is_java_class_array(ty: Ty) -> bool {
    let ty = ty.non_null();
    ty.is_reference_array() && ty.array_elem().is_some_and(is_java_class)
}

/// An annotation getter left `physical` on the stack. When that value is the stored
/// `java.lang.Class` of a `KClass` member, rebuild the `KClass` and return the type now on the
/// stack. Any other pair is left untouched: a property whose Kotlin type is already `Class`, or
/// whose getter returns `KClass`, is not an annotation-storage boundary.
pub(crate) fn adapt_read_kclass(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    logical: Ty,
    physical: Ty,
) -> Ty {
    if is_annotation_kclass(logical) && is_java_class(physical) {
        emit_class_to_kclass(cw, code);
        Ty::obj("kotlin/reflect/KClass")
    } else if is_annotation_kclass_array(logical) && is_java_class_array(physical) {
        emit_classes_to_kclasses(cw, code);
        Ty::array(Ty::obj("kotlin/reflect/KClass"))
    } else {
        physical
    }
}

/// Rebuild the `KClass` an annotation getter's `java.lang.Class` stands for.
pub(crate) fn emit_class_to_kclass(cw: &mut ClassWriter, code: &mut CodeBuilder) {
    let method = cw.methodref(
        "kotlin/jvm/internal/Reflection",
        "getOrCreateKotlinClass",
        "(Ljava/lang/Class;)Lkotlin/reflect/KClass;",
    );
    code.invokestatic(method, 1, 1);
}

/// Rebuild `KClass`s from a `java.lang.Class` array an annotation getter returned.
pub(crate) fn emit_classes_to_kclasses(cw: &mut ClassWriter, code: &mut CodeBuilder) {
    let method = cw.methodref(
        "kotlin/jvm/internal/Reflection",
        "getOrCreateKotlinClasses",
        "([Ljava/lang/Class;)[Lkotlin/reflect/KClass;",
    );
    code.invokestatic(method, 1, 1);
}

/// Copy `KClass[]` at `source` into a fresh `Class[]`, left in `result`.
///
/// `index` and `size` are int locals. The three slots are distinct from `source` and from each
/// other. The stack is empty on entry and on exit.
pub(crate) fn emit_kclass_array_to_class_array(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    source: u16,
    index: u16,
    size: u16,
    result: u16,
) {
    code.aload(source);
    code.arraylength();
    code.istore(size);
    code.iload(size);
    let class_type = cw.class_ref("java/lang/Class");
    code.anewarray(class_type);
    code.astore(result);
    code.push_int(0, cw);
    code.istore(index);
    let head = code.new_label();
    let done = code.new_label();
    code.bind(head);
    code.iload(index);
    code.iload(size);
    code.if_icmpge(done);
    code.aload(result);
    code.iload(index);
    code.aload(source);
    code.iload(index);
    code.array_load(0x32, 1);
    emit_kclass_to_class(cw, code);
    code.array_store(0x53, 1);
    code.iinc(index, 1);
    code.goto(head);
    code.bind(done);
}

/// Owner of `invokevirtual hashCode` for an annotation member's stored type.
///
/// The declared class owns the call (`String.hashCode`, `E.hashCode`) except when that class is
/// an interface — `invokevirtual` on an interface is illegal — or is `java.lang.Class`, which
/// declares no `hashCode` of its own. Both use `Object.hashCode`.
pub(crate) fn annotation_hash_owner(
    ir: &IrFile,
    symbols: &dyn BackendClassifierSource,
    ty: Ty,
) -> String {
    let Some(internal) = ty.obj_internal() else {
        return "java/lang/Object".to_string();
    };
    if internal == java_class_classifier() {
        return "java/lang/Object".to_string();
    }
    let interface = ir
        .classes
        .iter()
        .any(|class| class.fq_name == internal && (class.is_interface || class.is_annotation))
        || symbols
            .classifier(internal)
            .is_some_and(|classifier| classifier.is_interface());
    if interface {
        "java/lang/Object".to_string()
    } else {
        crate::jvm::names::classfile_internal_name_of(internal).to_string()
    }
}
