//! The declaration shape of an annotation class read from the classpath: the elements a Java
//! `@interface` exposes as an application's parameters, and the targets an application written
//! without a use-site prefix may land on.

use crate::jvm::classreader::{ClassInfo, MethodSig};
use crate::libraries::{
    AnnotationParameterPolicy, AnnotationPositionalPolicy, ParamList, Visibility,
};
use crate::types::{AnnotationTargets, Ty, TyKind};

/// Normalize a constructor-less annotation declaration into the common application shape while
/// JVM descriptors and `AnnotationDefault` are still provider-owned facts. Core never reparses a
/// descriptor or asks whether this classifier came from Java.
pub(super) fn java_annotation_parameter_list(class: &ClassInfo) -> Option<ParamList> {
    let mut elements = class
        .methods
        .iter()
        .filter(|method| is_java_annotation_element(method))
        .map(|method| {
            java_annotation_element_type(method)
                .map(|ty| (method.name.clone(), ty, method.has_annotation_default))
        })
        .collect::<Option<Vec<_>>>()?;
    let value_index = elements.iter().position(|(name, _, _)| name == "value");
    let value_vararg = value_index.is_some_and(|index| elements[index].1.array_elem().is_some());
    if let Some(index) = value_index {
        let value = elements.remove(index);
        elements.insert(0, value);
    }
    let positional = match (value_index, value_vararg) {
        (_, true) => AnnotationPositionalPolicy::ValueVararg,
        (Some(_), false) => AnnotationPositionalPolicy::Value,
        (None, false) => AnnotationPositionalPolicy::NamedOnly,
    };
    Some(ParamList {
        visibility: Visibility::Public,
        names: elements.iter().map(|(name, _, _)| name.clone()).collect(),
        defaults: elements.iter().map(|(_, _, default)| *default).collect(),
        types: elements.iter().map(|(_, ty, _)| *ty).collect(),
        recv_fun: vec![false; elements.len()].into(),
        vararg: value_vararg.then_some(0),
        annotation: Some(AnnotationParameterPolicy {
            positional,
            materialize_omitted_vararg: false,
        }),
    })
}

/// Whether `method` is an element of the Java `@interface` that declares it.
pub(super) fn is_java_annotation_element(method: &MethodSig) -> bool {
    method.is_public() && !method.is_static() && method.name != "<init>"
}

/// The Kotlin type of the Java annotation element `method`, or `None` when its descriptor cannot be
/// read or it takes parameters.
pub(super) fn java_annotation_element_type(method: &MethodSig) -> Option<Ty> {
    // Java exposes a class-valued annotation element as `Class<T>`, while Kotlin application syntax
    // supplies a `KClass<T>`. Keep the generic argument decoded from the method's authoritative
    // `Signature` attribute: dropping it would accept `String::class` for
    // `Class<? extends Runnable>`. The physical descriptor remains provider-owned and unchanged.
    let kotlin_class_element = |semantic: Ty| {
        let arguments = match semantic.non_null().kind() {
            TyKind::Obj(_, arguments) => arguments,
            _ => crate::types::TyList::EMPTY,
        };
        Ty::obj_args_name(
            crate::types::type_name("kotlin/reflect/KClass"),
            arguments.as_slice(),
        )
    };
    let (parameters, ret) = crate::jvm::names::parse_method_descriptor(&method.descriptor)?;
    if !parameters.is_empty() {
        return None;
    }
    let erased = super::declared_desc_to_ty(ret);
    let generic = method
        .signature
        .as_deref()
        .and_then(super::parse_method_gsig)
        .map(|signature| signature.ret);
    let ty = match erased.array_elem() {
        Some(erased_element) if erased_element.non_null() == Ty::obj("java/lang/Class") => {
            let element = generic.and_then(Ty::array_elem).unwrap_or(erased_element);
            Ty::array(kotlin_class_element(element))
        }
        None if erased.non_null() == Ty::obj("java/lang/Class") => {
            kotlin_class_element(generic.unwrap_or(erased))
        }
        Some(_) | None => erased,
    };
    (ty != Ty::Error).then_some(ty)
}

/// Where an application of this classpath annotation, written with no use-site prefix, may land.
///
/// A KOTLIN annotation carries `@kotlin.annotation.Target(allowedTargets = …)` — the only form that
/// can name `PROPERTY` — and declaring none means "applicable everywhere". A JAVA `@interface`
/// carries `@java.lang.annotation.Target(ElementType…)`, and Java has no property: kotlinc places a
/// bare Java annotation written on a Kotlin property on its parameter or backing field instead.
pub(super) fn classpath_annotation_targets(ci: &ClassInfo) -> AnnotationTargets {
    if !ci.kotlin_targets.is_empty() {
        return AnnotationTargets::kotlin(ci.kotlin_targets.iter().filter_map(|entry| {
            // The class reader kept only `kotlin.annotation.AnnotationTarget` elements.
            crate::types::KotlinTarget::of_entry(
                crate::types::type_name("kotlin/annotation/AnnotationTarget"),
                entry,
            )
        }));
    }
    // A Kotlin annotation is identified by its own `@Metadata`; without a declared `@Target` it is
    // applicable everywhere.
    if ci.meta.class_kind.is_some() {
        return AnnotationTargets::DEFAULT;
    }
    AnnotationTargets::java(&ci.java_targets)
}
