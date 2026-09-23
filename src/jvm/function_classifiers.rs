//! JVM realization adapter for compiler-provided Kotlin function classifiers.
//!
//! The common library module owns the source declaration. This adapter only chooses a seed that
//! carries JVM external-call identities when a fixed runtime interface exists.

use std::sync::Arc;

use crate::libraries::{function_classifiers as common, LibraryType};
use crate::types::{type_name, type_name_child, TypeName};

pub(super) fn classifier(internal: TypeName) -> Option<common::FunctionClassifier> {
    common::classifier(internal)
}

pub(super) fn classifier_name(fqn: &str) -> Option<TypeName> {
    common::classifier_name(fqn)
}

pub(crate) fn is_reflective_function_classifier(internal: TypeName) -> bool {
    common::is_reflective_function_classifier(internal)
}

pub(super) fn build(
    function: common::FunctionClassifier,
    mut resolve: impl FnMut(TypeName) -> Option<Arc<LibraryType>>,
) -> Option<LibraryType> {
    let physical_seed = if function.is_reflective() {
        resolve(type_name(crate::types::KFUNCTION_INTERNAL))
    } else {
        // Suspend functions use a continuation parameter in the JVM runtime interface. This is a
        // physical seed choice only; common normalization below restores the source arity and
        // semantic suspend signature.
        let runtime_arity = function.arity() + usize::from(function.is_suspend());
        resolve(type_name_child(
            type_name("kotlin/jvm/functions"),
            &format!("Function{runtime_arity}"),
        ))
    };
    let seed = match physical_seed {
        Some(seed) => seed,
        None => common::synthetic(function),
    };
    Some(common::normalize(function, (*seed).clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbol_source::SymbolSource;

    #[test]
    fn large_arity_classifier_keeps_common_shape_without_a_fixed_jvm_interface() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib]),
        ))
        .expect("JVM provider initialization");
        let classifier = libraries
            .classifier(type_name("kotlin/Function30"))
            .expect("semantic Function30 classifier");
        let crate::types::Ty::Fun(signature) = classifier
            .callable_signature
            .expect("Function30 callable shape")
        else {
            panic!("Function30 callable signature is not a function")
        };
        assert_eq!(classifier.type_params().len(), 31);
        assert_eq!(signature.params.len(), 30);
        assert!(classifier.represents_function_type());
    }
}
