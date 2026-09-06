//! Kotlin compiler-provided function classifier declarations.
//!
//! `FunctionN`, `SuspendFunctionN`, and their reflective counterparts are source-language
//! declarations supplied by Kotlin's builtins provider. Fixed JVM `Function0..22` interfaces are
//! one possible realization, not the semantic declaration boundary. This module normalizes the
//! family into ordinary [`LibraryType`] records so resolution never derives callable shape from a
//! source spelling and common FIR remains independent of JVM arity representation.

use crate::libraries::LibraryType;
use crate::types::{type_name, type_name_child, Ty, TypeName, TypeVariance};

#[derive(Clone, Copy, PartialEq, Eq)]
enum FunctionClassKind {
    Function,
    KFunction,
    SuspendFunction,
    KSuspendFunction,
}

#[derive(Clone, Copy)]
pub(super) struct FunctionClassifier {
    kind: FunctionClassKind,
    arity: usize,
}

/// Recognize a declaration name owned by Kotlin's function-class builtins provider.
pub(super) fn classifier(internal: TypeName) -> Option<FunctionClassifier> {
    let package = internal.parent()?;
    let (kind, digits) = if package == type_name("kotlin") {
        (
            FunctionClassKind::Function,
            internal.segment_ref().strip_prefix("Function")?,
        )
    } else if package == type_name("kotlin/coroutines") {
        (
            FunctionClassKind::SuspendFunction,
            internal.segment_ref().strip_prefix("SuspendFunction")?,
        )
    } else if package == type_name("kotlin/reflect") {
        if let Some(digits) = internal.segment_ref().strip_prefix("KSuspendFunction") {
            (FunctionClassKind::KSuspendFunction, digits)
        } else {
            (
                FunctionClassKind::KFunction,
                internal.segment_ref().strip_prefix("KFunction")?,
            )
        }
    } else {
        return None;
    };
    let arity = digits.parse().ok()?;
    Some(FunctionClassifier { kind, arity })
}

/// Intern a provider-owned classifier name only after its complete namespace and numeric family
/// have been recognized. Arbitrary lookup spellings never enter the name interner.
pub(super) fn classifier_name(fqn: &str) -> Option<TypeName> {
    let (package, name) = fqn.rsplit_once('/')?;
    let digits = match package {
        "kotlin" => name.strip_prefix("Function")?,
        "kotlin/coroutines" => name.strip_prefix("SuspendFunction")?,
        "kotlin/reflect" => name
            .strip_prefix("KSuspendFunction")
            .or_else(|| name.strip_prefix("KFunction"))?,
        _ => return None,
    };
    (!digits.is_empty() && digits.bytes().all(|digit| digit.is_ascii_digit())).then_some(())?;
    Some(type_name(fqn))
}

pub(crate) fn is_reflective_function_classifier(internal: TypeName) -> bool {
    classifier(internal).is_some_and(|function| {
        matches!(
            function.kind,
            FunctionClassKind::KFunction | FunctionClassKind::KSuspendFunction
        )
    })
}

fn type_parameters(arity: usize) -> (Vec<String>, Vec<Ty>, Vec<Vec<Ty>>, Vec<TypeVariance>) {
    let mut names = (1..=arity)
        .map(|index| format!("P{index}"))
        .collect::<Vec<_>>();
    names.push("R".to_string());
    let upper = Ty::nullable(Ty::obj("kotlin/Any"));
    let arguments = names
        .iter()
        .map(|formal| Ty::ty_param(formal, upper))
        .collect::<Vec<_>>();
    let bounds = vec![vec![upper]; arguments.len()];
    let mut variances = vec![TypeVariance::In; arity];
    variances.push(TypeVariance::Out);
    (names, arguments, bounds, variances)
}

fn ordinary_shape(
    arity: usize,
    resolve: &mut impl FnMut(TypeName) -> Option<std::sync::Arc<LibraryType>>,
) -> Option<LibraryType> {
    let runtime = type_name_child(
        type_name("kotlin/jvm/functions"),
        &format!("Function{arity}"),
    );
    if let Some(classifier) = resolve(runtime) {
        return Some((*classifier).clone());
    }

    // The JVM has fixed generic interfaces only through Function22. Larger arities still have a
    // complete Kotlin declaration; start from the semantic Function base and publish its generated
    // parameters/callable signature without choosing a runtime representation.
    let (names, arguments, bounds, variances) = type_parameters(arity);
    let result = *arguments.last()?;
    let mut shape = (*resolve(type_name("kotlin/Function"))?).clone();
    shape.type_parameters = crate::types::TypeParameters::new(names, bounds, variances);
    shape.callable_signature = Some(Ty::fun(arguments[..arity].to_vec(), result));
    shape.callable_signatures = vec![shape.callable_signature.expect("function shape")];
    shape.supertypes = vec![type_name("kotlin/Function")].into();
    shape.supertype_templates = vec![Ty::obj_args("kotlin/Function", &[result])];
    Some(shape)
}

/// Construct the common semantic record for one recognized function classifier. `resolve` supplies
/// existing builtin/base declarations; this function never reads classfiles or selects an ABI.
pub(super) fn build(
    function: FunctionClassifier,
    mut resolve: impl FnMut(TypeName) -> Option<std::sync::Arc<LibraryType>>,
) -> Option<LibraryType> {
    if function.kind == FunctionClassKind::Function {
        return ordinary_shape(function.arity, &mut resolve);
    }

    let suspend = matches!(
        function.kind,
        FunctionClassKind::SuspendFunction | FunctionClassKind::KSuspendFunction
    );
    let reflective = matches!(
        function.kind,
        FunctionClassKind::KFunction | FunctionClassKind::KSuspendFunction
    );
    // A suspend function's runtime callable interface has one CPS parameter beyond its source
    // arity. Use the ordinary provider shape as the declaration template so this also works above
    // the JVM fixed-interface limit.
    let runtime_arity = function.arity + usize::from(suspend);
    let runtime_shape = ordinary_shape(runtime_arity, &mut resolve)?;
    let (type_params, arguments, type_param_bounds, type_param_variances) =
        type_parameters(function.arity);
    let result = *arguments.last()?;
    let callable = Ty::fun_with_shape(
        arguments[..function.arity].to_vec(),
        result,
        0,
        false,
        suspend,
    );
    let mut shape = if reflective {
        (*resolve(type_name(crate::types::KFUNCTION_INTERNAL))?).clone()
    } else {
        runtime_shape
    };
    shape.type_parameters =
        crate::types::TypeParameters::new(type_params, type_param_bounds, type_param_variances);
    shape.callable_signature = Some(callable);
    shape.callable_signatures = vec![callable];
    if reflective {
        let callable_classifier = if suspend {
            type_name_child(
                type_name("kotlin/coroutines"),
                &format!("SuspendFunction{}", function.arity),
            )
        } else {
            type_name_child(type_name("kotlin"), &format!("Function{}", function.arity))
        };
        shape.supertypes = vec![
            type_name(crate::types::KFUNCTION_INTERNAL),
            callable_classifier,
        ]
        .into();
        shape.supertype_templates = vec![
            Ty::obj_args(crate::types::KFUNCTION_INTERNAL, &[result]),
            Ty::obj_args_name(callable_classifier, &arguments),
        ];
    } else {
        shape.supertypes = vec![type_name("kotlin/Function")].into();
        shape.supertype_templates = vec![Ty::obj_args("kotlin/Function", &[result])];
    }
    Some(shape)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbol_source::SymbolSource;

    #[test]
    fn recognizes_only_the_builtin_classifier_namespaces() {
        assert!(classifier(type_name("kotlin/reflect/KFunction23")).is_some());
        assert!(classifier(type_name("kotlin/coroutines/SuspendFunction1")).is_some());
        assert!(classifier(type_name("kotlin/reflect/KSuspendFunction1")).is_some());
        assert!(classifier(type_name("kotlin/reflect/KFunction")).is_none());
        assert!(classifier(type_name("kotlin/coroutines/SuspendFunction")).is_none());
        assert!(classifier(type_name("other/KFunction1")).is_none());
        assert!(classifier(type_name("kotlin/reflect/KFunctionX")).is_none());
    }

    #[test]
    fn large_arity_classifier_has_a_semantic_provider_shape() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let libraries = crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(vec![stdlib]),
        ));
        let classifier = libraries
            .classifier(type_name("kotlin/Function30"))
            .expect("semantic Function30 classifier");
        let Ty::Fun(signature) = classifier
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
