//! When a function's `JvmMethodSignature` must be recorded: kotlinc's
//! `FirJvmSignatureSerializer.requiresFunctionSignature`. A reader rebuilds the descriptor of a
//! function without one by mapping the class id of each declared type (extension receiver, value
//! parameters, return) through `ClassMapperLite`; the signature is written exactly when that
//! reconstruction is impossible or differs from the physical descriptor.

use std::collections::HashSet;

use crate::types::{Ty, TypeName};

/// `requiresFunctionSignature`: `value_parameters` exclude context parameters, which the
/// reconstruction never sees, so a function with any records its descriptor. A classifier in
/// `local_classifiers` has a class id no JVM name derives from.
pub(super) fn requires_function_signature(
    receiver: Option<Ty>,
    value_parameters: impl IntoIterator<Item = Ty>,
    ret: Ty,
    physical: &str,
    local_classifiers: &HashSet<TypeName>,
) -> bool {
    let mapped = |ty| map_type_default(ty, local_classifiers);
    let mut derived = String::from("(");
    for ty in receiver.into_iter().chain(value_parameters) {
        let Some(descriptor) = mapped(ty) else {
            return true;
        };
        derived.push_str(&descriptor);
    }
    derived.push(')');
    let Some(result) = mapped(ret) else {
        return true;
    };
    derived.push_str(&result);
    derived != physical
}

/// `mapTypeDefault`: the descriptor `ClassMapperLite` gives the type's class id, nullability
/// ignored, or `None` for a type with no class id (a type parameter) or a local one.
fn map_type_default(ty: Ty, local_classifiers: &HashSet<TypeName>) -> Option<String> {
    Some(match ty.non_null() {
        Ty::Obj(classifier, _) if local_classifiers.contains(&classifier) => return None,
        Ty::Obj(classifier, _) => super::jvm_class_map::class_mapper_lite_descriptor(classifier),
        Ty::Unit => "V".to_owned(),
        Ty::Nothing => super::jvm_class_map::class_mapper_lite_nothing_descriptor(),
        // FIR's class id of a function type is its kind and arity, receiver and context included.
        Ty::Fun(signature) => super::jvm_class_map::class_mapper_lite_function_descriptor(
            signature.params.len(),
            signature.suspend,
        ),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nullable_primitive_or_an_erased_reference_records_its_descriptor() {
        let none = HashSet::new();
        assert!(!requires_function_signature(
            None,
            [Ty::Int],
            Ty::Unit,
            "(I)V",
            &none
        ));
        assert!(requires_function_signature(
            None,
            [Ty::nullable(Ty::Int)],
            Ty::Unit,
            "(Ljava/lang/Integer;)V",
            &none
        ));
        let reflective = Ty::obj_args("kotlin/reflect/KSuspendFunction0", &[Ty::Unit]);
        assert!(requires_function_signature(
            Some(reflective),
            [],
            Ty::String,
            "(Lkotlin/reflect/KFunction;)Ljava/lang/String;",
            &none
        ));
    }
}
