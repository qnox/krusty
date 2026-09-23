//! Kotlin compiler-provided function classifier declarations.
//!
//! `FunctionN`, `SuspendFunctionN`, and their reflective counterparts are source-language
//! declarations supplied by Kotlin's builtins provider. A target may seed the record with a
//! physical realization, but the classifier family, generic variance, callable shape, and Kotlin
//! supertypes are stated here once. Resolution therefore never derives them from a backend ABI.

use std::sync::Arc;

use crate::libraries::{
    CallSig, Callables, ClassifierInheritance, FnKind, FunctionInfo, FunctionSet, LibraryCallable,
    LibraryMember, LibraryType, TypeKind,
};
use crate::types::{type_name, type_name_child, Ty, TypeName, TypeVariance};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FunctionClassKind {
    Function,
    KFunction,
    SuspendFunction,
    KSuspendFunction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FunctionClassifier {
    kind: FunctionClassKind,
    arity: usize,
}

impl FunctionClassifier {
    pub(crate) fn arity(self) -> usize {
        self.arity
    }

    pub(crate) fn is_suspend(self) -> bool {
        matches!(
            self.kind,
            FunctionClassKind::SuspendFunction | FunctionClassKind::KSuspendFunction
        )
    }

    pub(crate) fn is_reflective(self) -> bool {
        matches!(
            self.kind,
            FunctionClassKind::KFunction | FunctionClassKind::KSuspendFunction
        )
    }

    pub(crate) fn identity(self) -> TypeName {
        let (package, prefix) = match self.kind {
            FunctionClassKind::Function => ("kotlin", "Function"),
            FunctionClassKind::SuspendFunction => ("kotlin/coroutines", "SuspendFunction"),
            FunctionClassKind::KFunction => ("kotlin/reflect", "KFunction"),
            FunctionClassKind::KSuspendFunction => ("kotlin/reflect", "KSuspendFunction"),
        };
        type_name_child(type_name(package), &format!("{prefix}{}", self.arity))
    }
}

/// Recognize a declaration identity owned by Kotlin's function-class builtins provider.
pub(crate) fn classifier(internal: TypeName) -> Option<FunctionClassifier> {
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

/// Intern a provider-owned classifier identity only after its complete namespace and numeric family
/// have been recognized. Arbitrary lookup spellings never enter the name interner.
pub(crate) fn classifier_name(fqn: &str) -> Option<TypeName> {
    let (package, name) = fqn.rsplit_once('/')?;
    let digits = match package {
        "kotlin" => name.strip_prefix("Function")?,
        "kotlin/coroutines" => name.strip_prefix("SuspendFunction")?,
        "kotlin/reflect" => match name.strip_prefix("KSuspendFunction") {
            Some(digits) => digits,
            None => name.strip_prefix("KFunction")?,
        },
        _ => return None,
    };
    (!digits.is_empty() && digits.bytes().all(|digit| digit.is_ascii_digit())).then_some(())?;
    Some(type_name(fqn))
}

pub(crate) fn is_reflective_function_classifier(internal: TypeName) -> bool {
    classifier(internal).is_some_and(FunctionClassifier::is_reflective)
}

pub(crate) fn function_type(arity: usize) -> Ty {
    Ty::obj_name(
        FunctionClassifier {
            kind: FunctionClassKind::Function,
            arity,
        }
        .identity(),
    )
}

pub(crate) fn property_reference_type(arity: usize, mutable: bool, args: &[Ty]) -> Option<Ty> {
    let prefix = if mutable {
        "KMutableProperty"
    } else {
        "KProperty"
    };
    if arity > 2 || args.len() != arity + 1 || args.contains(&Ty::Error) {
        return None;
    }
    let classifier = type_name_child(type_name("kotlin/reflect"), &format!("{prefix}{arity}"));
    Some(Ty::obj_args_name(classifier, args))
}

/// Select the reflective classifier from an already-resolved function signature. Direction matters:
/// consumers never parse a reflective classifier spelling to reconstruct callable semantics.
pub(crate) fn function_reference_type(function: Ty) -> Option<Ty> {
    let Ty::Fun(signature) = function else {
        return None;
    };
    if signature.context_count != 0 {
        return None;
    }
    let mut arguments = signature.params.to_vec();
    arguments.push(signature.ret);
    let classifier = FunctionClassifier {
        kind: if signature.suspend {
            FunctionClassKind::KSuspendFunction
        } else {
            FunctionClassKind::KFunction
        },
        arity: signature.params.len(),
    }
    .identity();
    Some(Ty::obj_args_name(classifier, &arguments))
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

fn ensure_invoke(
    shape: &mut LibraryType,
    identity: TypeName,
    parameters: &[Ty],
    result: Ty,
    suspend: bool,
) {
    let already_declared = shape
        .declared_callables
        .get("invoke")
        .is_some_and(|callables| {
            let (functions, _) = callables.clone().into_parts();
            functions.overloads.iter().any(|candidate| {
                candidate.semantic_params().as_ref() == parameters
                    && candidate.callable.ret.canonical_semantic() == result
            })
        });
    if !already_declared {
        let mut callable = LibraryCallable::library(
            identity,
            "invoke",
            parameters.to_vec(),
            result,
            result,
            String::new(),
        );
        callable.owner_is_interface = true;
        callable.suspend = suspend;
        let mut declaration =
            FunctionInfo::plain(FnKind::Member, Some(Ty::obj_name(identity)), callable);
        declaration.call_sig = CallSig::metadata_plain(parameters.len());
        declaration.flags.operator = true;
        declaration.flags.is_abstract = true;
        shape.insert_declared_callables(
            "invoke".to_string(),
            Callables::Functions(FunctionSet {
                overloads: vec![declaration],
            }),
        );
    }
    if !shape.members.iter().any(|member| {
        member.name == "invoke" && member.params == parameters && member.ret == result
    }) {
        let mut member = LibraryMember::new(
            "invoke".to_string(),
            parameters.to_vec(),
            result,
            String::new(),
        );
        member.owner = Some(identity);
        member.set_is_abstract(true);
        member.set_is_interface(true);
        member.set_is_operator(true);
        member.call_sig = CallSig::metadata_plain(parameters.len());
        shape.members.push(member);
    }
}

/// Normalize a provider-selected seed into the common semantic declaration for this function
/// classifier. The seed may retain opaque physical callable handles; every source-visible fact is
/// overwritten here from the compiler-owned family definition.
pub(crate) fn normalize(function: FunctionClassifier, mut shape: LibraryType) -> LibraryType {
    let (names, arguments, bounds, variances) = type_parameters(function.arity);
    let result = *arguments.last().expect("function result parameter");
    let callable = Ty::fun_with_shape(
        arguments[..function.arity].to_vec(),
        result,
        0,
        false,
        function.is_suspend(),
    );
    shape.is_kotlin = true;
    shape.kind = TypeKind::Interface;
    shape.inheritance = ClassifierInheritance {
        is_abstract: true,
        is_extensible: true,
        has_no_arg_constructor: false,
    };
    shape.type_parameters = crate::types::TypeParameters::new(names, bounds, variances);
    shape.own_type_parameter_count = function.arity + 1;
    shape.callable_signature = Some(callable);
    shape.callable_signatures = vec![callable];
    if function.is_reflective() {
        let callable_classifier = FunctionClassifier {
            kind: if function.is_suspend() {
                FunctionClassKind::SuspendFunction
            } else {
                FunctionClassKind::Function
            },
            arity: function.arity,
        }
        .identity();
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
        ensure_invoke(
            &mut shape,
            function.identity(),
            &arguments[..function.arity],
            result,
            function.is_suspend(),
        );
    }
    shape
}

/// Build the declaration without a target realization. Providers that have no physical function
/// interface use this record directly; target adapters may instead pass a seed carrying opaque emit
/// identities to [`normalize`].
pub(crate) fn synthetic(function: FunctionClassifier) -> Arc<LibraryType> {
    Arc::new(normalize(function, LibraryType::declaration_header()))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn synthetic_function_has_complete_common_shape() {
        let function = classifier(type_name("kotlin/Function30")).expect("Function30 identity");
        let shape = synthetic(function);
        let Ty::Fun(signature) = shape.callable_signature.expect("callable shape") else {
            panic!("Function30 callable signature is not a function")
        };
        assert_eq!(shape.type_params().len(), 31);
        assert_eq!(signature.params.len(), 30);
        assert!(shape.represents_function_type());
        assert!(shape.declared_callables.contains_key("invoke"));
    }

    #[test]
    fn references_are_selected_from_the_resolved_signature() {
        let function = Ty::fun_with_shape(vec![Ty::Int], Ty::String, 0, false, true);
        assert_eq!(
            function_reference_type(function),
            Some(Ty::obj_args(
                "kotlin/reflect/KSuspendFunction1",
                &[Ty::Int, Ty::String]
            ))
        );
        assert_eq!(
            property_reference_type(1, false, &[Ty::String, Ty::Int]),
            Some(Ty::obj_args(
                "kotlin/reflect/KProperty1",
                &[Ty::String, Ty::Int]
            ))
        );
    }
}
