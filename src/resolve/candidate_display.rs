//! Source rendering of callable and classifier candidates for overload diagnostics.
//!
//! Every candidate is rendered from its semantic record. A function reads
//! `fun <T> R.name(p: P): Ret`; a constructor collected into a receiver's member level reads as
//! kotlinc prints it, `Outer.constructor(p: P): Outer.Inner`. A classifier reads as its declaration
//! header, `class Box<T> : Any`, and a typealias as `typealias Name<T> = Expansion<T>`.
//! Declaration origin affects neither shape nor text.

use super::*;

impl Checker<'_> {
    /// Render a callable directly from its semantic record. The receiver is an attribute of an
    /// extension, never positional parameter zero.
    pub(super) fn callable_candidate_display(
        name: &str,
        function: &crate::libraries::FunctionInfo,
    ) -> String {
        match function.bound_inner_constructor {
            Some(constructor) => Self::bound_inner_constructor_display(constructor, function),
            None => Self::generic_callable_display(name, function),
        }
    }

    pub(super) fn generic_callable_display(
        name: &str,
        function: &crate::libraries::FunctionInfo,
    ) -> String {
        let signature = function.semantic_signature();
        Self::semantic_callable_display(
            name,
            &signature,
            &function.call_sig,
            function.context_count,
            function.callable.suspend,
            function.flags.reified,
            function
                .is_extension()
                .then(|| function.semantic_receiver())
                .flatten(),
        )
    }

    pub(super) fn library_member_candidate_display(
        name: &str,
        member: &crate::libraries::LibraryMember,
    ) -> String {
        let signature = member.generic_sig.as_ref().map_or_else(
            || {
                std::borrow::Cow::Owned(crate::libraries::GenericSig {
                    formals: Vec::new(),
                    formal_bounds: Vec::new(),
                    receiver: None,
                    params: member.params.clone(),
                    ret: member.ret,
                    return_policy: crate::libraries::GenericReturnPolicy::Exact,
                })
            },
            std::borrow::Cow::Borrowed,
        );
        Self::semantic_callable_display(
            name,
            &signature,
            &member.call_sig,
            member.context_count,
            member.suspend(),
            member.reified,
            None,
        )
    }

    pub(super) fn semantic_callable_display(
        name: &str,
        signature: &crate::libraries::GenericSig,
        call_sig: &CallSig,
        context_count: usize,
        suspend: bool,
        reified: bool,
        extension_receiver: Option<Ty>,
    ) -> String {
        let type_parameters = type_parameters_display(signature, reified);
        let parameters = parameters_display(signature, call_sig);
        let context_count = context_count.min(parameters.len());
        let context = if context_count == 0 {
            String::new()
        } else {
            format!("context({}) ", parameters[..context_count].join(", "))
        };
        let parameters = parameters[context_count..].join(", ");
        let suspend = if suspend { "suspend " } else { "" };
        let receiver = extension_receiver
            .map(|receiver| format!("{}.", receiver.source_name()))
            .unwrap_or_default();
        format!(
            "{context}{suspend}fun {type_parameters}{receiver}{name}({parameters}): {}",
            signature.ret.source_name()
        )
    }

    /// kotlinc names a constructor by the classifier that declares its outer instance, not by the
    /// receiver the call went through: `Base.constructor(x: Int): Base.Inner` for `derived.Inner(1)`.
    fn bound_inner_constructor_display(
        constructor: crate::libraries::BoundInnerConstructor,
        function: &crate::libraries::FunctionInfo,
    ) -> String {
        let signature = function.semantic_signature();
        let type_parameters = type_parameters_display(&signature, false);
        let type_parameters = type_parameters.trim_end();
        let parameters = parameters_display(&signature, &function.call_sig);
        let parameters = parameters[function.context_count.min(parameters.len())..].join(", ");
        format!(
            "{}.constructor{type_parameters}({parameters}): {}",
            Ty::obj_name(constructor.outer).source_name(),
            signature.ret.source_name(),
        )
    }
}

/// `<T : Bound, U> ` with its trailing separator, or empty for a non-generic signature.
fn type_parameters_display(signature: &crate::libraries::GenericSig, reified: bool) -> String {
    let type_parameters = signature
        .formals
        .iter()
        .enumerate()
        .map(|(index, formal)| {
            let source = crate::types::type_parameter_source_name(formal);
            let reified = if reified && index == 0 {
                "reified "
            } else {
                ""
            };
            let bound = signature
                .formal_bounds
                .get(index)
                .and_then(|bounds| bounds.first())
                .copied()
                .map(|bound| format!(" : {}", bound.source_name()))
                .unwrap_or_default();
            format!("{reified}{source}{bound}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    if type_parameters.is_empty() {
        String::new()
    } else {
        format!("<{type_parameters}> ")
    }
}

/// Each declared parameter as `vararg name: Type = ...`, context parameters included.
fn parameters_display(signature: &crate::libraries::GenericSig, call_sig: &CallSig) -> Vec<String> {
    signature
        .params
        .iter()
        .enumerate()
        .map(|(index, parameter)| {
            let parameter_name = call_sig
                .param_names
                .get(index)
                .map(String::as_str)
                .unwrap_or("_");
            let default = if call_sig.param_defaults.get(index).copied().unwrap_or(false) {
                " = ..."
            } else {
                ""
            };
            let vararg = if call_sig.vararg_index == Some(index) {
                "vararg "
            } else {
                ""
            };
            format!(
                "{vararg}{parameter_name}: {}{default}",
                parameter.source_name()
            )
        })
        .collect()
}

/// kotlinc's `OVERLOAD_RESOLUTION_AMBIGUITY` for a classifier name: each equally visible
/// classifier rendered as its declaration header, in candidate order. A candidate named through a
/// typealias renders as that alias declaration.
pub(super) fn ambiguous_classifier_message<
    Shape: std::ops::Deref<Target = crate::libraries::LibraryType>,
>(
    candidates: &[crate::symbol_resolver::ScopedClassifier],
    shape: impl Fn(TypeName) -> Option<Shape>,
) -> String {
    let mut message = "overload resolution ambiguity between candidates:".to_string();
    for candidate in candidates {
        let display = match &candidate.alias {
            Some(alias) => Some(type_alias_display(alias)),
            None => shape(candidate.classifier)
                .map(|shape| classifier_access_display_from_shape(candidate.classifier, &shape)),
        };
        if let Some(display) = display {
            message.push('\n');
            message.push_str(&display);
        }
    }
    message
}

/// kotlinc's declaration header of a typealias: `typealias Name<T> = Expansion<T>`.
fn type_alias_display(alias: &crate::libraries::AliasExpansion) -> String {
    let formals = if alias.formals.is_empty() {
        String::new()
    } else {
        format!(
            "<{}>",
            alias
                .formals
                .iter()
                .map(|formal| crate::types::type_parameter_source_name(formal))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "typealias {}{formals} = {}",
        alias.identity.nested_segment_ref(),
        alias.expansion.source_name()
    )
}

/// kotlinc's descriptor rendering of a classifier: `class Helper : Any`, `class Box<out T> : Any`.
pub(super) fn classifier_access_display_from_shape(
    internal: TypeName,
    shape: &crate::libraries::LibraryType,
) -> String {
    let kind = match shape.kind {
        crate::libraries::TypeKind::Class => "class",
        crate::libraries::TypeKind::Interface => "interface",
        crate::libraries::TypeKind::Annotation => "annotation class",
        crate::libraries::TypeKind::Enum => "enum class",
        crate::libraries::TypeKind::Object => "object",
    };
    let supertypes = shape
        .supertypes
        .iter_ids()
        .map(|supertype| {
            let ty = Ty::obj_name(supertype);
            if ty.is_erased_top() {
                "Any".to_string()
            } else {
                ty.source_name()
            }
        })
        .collect::<Vec<_>>();
    let supertypes = if supertypes.is_empty() {
        String::new()
    } else {
        format!(" : {}", supertypes.join(", "))
    };
    let own_type_parameters = shape
        .type_params
        .iter()
        .zip(shape.type_param_variances.iter())
        .take(shape.own_type_parameter_count)
        .map(|(name, variance)| {
            let variance = match variance {
                crate::types::TypeVariance::Invariant => "",
                crate::types::TypeVariance::In => "in ",
                crate::types::TypeVariance::Out => "out ",
            };
            format!(
                "{variance}{}",
                crate::types::type_parameter_source_name(name)
            )
        })
        .collect::<Vec<_>>();
    let type_parameters = if own_type_parameters.is_empty() {
        String::new()
    } else {
        format!("<{}>", own_type_parameters.join(", "))
    };
    format!(
        "{} {}{type_parameters}{supertypes}",
        kind,
        internal.nested_segment_ref()
    )
}
