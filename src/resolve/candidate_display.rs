//! Source rendering of callable and classifier candidates for overload diagnostics.
//!
//! Every candidate is rendered from its semantic record. A function reads
//! `fun <T> R.name(p: P): Ret`; a constructor collected into a receiver's member level reads as
//! kotlinc prints it, `Outer.constructor(p: P): Outer.Inner`. A classifier reads as its declaration
//! header, `class Box<T> : Any`, and a typealias as `typealias Name<T> = Expansion<T>`.
//! Declaration origin affects neither shape nor text.

use super::*;

pub(super) fn inaccessible_classifier_message(
    name: &str,
    access: crate::symbol_source::ClassifierAccess,
) -> String {
    use crate::symbol_source::ClassifierAccess;

    let kind = match access {
        ClassifierAccess::Private => "private",
        ClassifierAccess::Protected => "protected",
        ClassifierAccess::Internal => "internal",
        ClassifierAccess::PackagePrivate => "package-private",
        ClassifierAccess::Public => "public",
    };
    format!("cannot access '{name}': it is {kind}")
}

impl Checker<'_> {
    /// Render a callable directly from its semantic record. The receiver is an attribute of an
    /// extension, never positional parameter zero.
    /// kotlinc's ambiguity diagnostic, listing the tied most specific `candidates`.
    pub(super) fn ambiguous_candidates_message<'f>(
        name: &str,
        candidates: impl IntoIterator<Item = &'f crate::libraries::FunctionInfo>,
    ) -> String {
        let mut message = "overload resolution ambiguity between candidates:".to_string();
        for candidate in candidates {
            message.push('\n');
            message.push_str(&Self::callable_candidate_display(name, candidate));
        }
        message
    }

    pub(super) fn callable_candidate_display(
        name: &str,
        function: &crate::libraries::FunctionInfo,
    ) -> String {
        Self::callable_candidate_display_with_alias_constructor(
            name,
            function,
            Self::generic_constructor_display,
        )
    }

    /// Retain the declaration-kind dispatch while a caller supplies alias parameter type rendering.
    pub(super) fn callable_candidate_display_with_alias_constructor(
        name: &str,
        function: &crate::libraries::FunctionInfo,
        constructor: impl Fn(&crate::libraries::GenericSig, &crate::libraries::LibraryMember) -> String,
    ) -> String {
        match function.bound_inner_constructor {
            Some(_) if function.constructor_alias.is_some() => {
                let signature = &function
                    .constructor_alias
                    .as_deref()
                    .expect("an alias constructor retains its declaration")
                    .signature;
                constructor(signature, &function.member_with_return(signature.ret))
            }
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

    /// `fun <T : Bound, U> name`, used inside diagnostics that identify one callable-owned type
    /// parameter without rendering its value parameters or result.
    pub(super) fn generic_callable_header(
        name: &str,
        function: &crate::libraries::FunctionInfo,
    ) -> String {
        let signature = function.semantic_signature();
        let type_parameters = type_parameters_display(&signature, function.flags.reified);
        format!("fun {type_parameters}{name}")
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

    /// `fun <T : Bound> name` for a provider-normalized member declaration.
    pub(super) fn generic_library_member_header(
        name: &str,
        member: &crate::libraries::LibraryMember,
    ) -> Option<String> {
        let signature = member.generic_sig.as_ref()?;
        let type_parameters = type_parameters_display(signature, member.reified);
        Some(format!("fun {type_parameters}{name}"))
    }

    /// kotlinc's rendering of a generic classifier's constructor,
    /// `constructor<T : Number>(t: T): Box<T>`: the classifier's type parameters follow the keyword.
    pub(super) fn generic_constructor_display(
        signature: &crate::libraries::GenericSig,
        member: &crate::libraries::LibraryMember,
    ) -> String {
        Self::generic_constructor_display_with_type(signature, member, &Ty::source_name)
    }

    pub(super) fn generic_constructor_display_with_type(
        signature: &crate::libraries::GenericSig,
        member: &crate::libraries::LibraryMember,
        render: &dyn Fn(Ty) -> String,
    ) -> String {
        let type_parameters = type_parameters_display(signature, false);
        let parameters = parameters_display_with_type(signature, &member.call_sig, render);
        let parameters = parameters[member.context_count.min(parameters.len())..].join(", ");
        format!(
            "constructor{}({parameters}): {}",
            type_parameters.trim_end(),
            signature.ret.source_name()
        )
    }

    pub(super) fn bound_constructor_type_parameter_owner(
        outer: Ty,
        signature: &crate::libraries::GenericSig,
    ) -> String {
        format!(
            "{}.constructor{}: {}",
            outer.source_name(),
            type_parameters_display(signature, false).trim_end(),
            signature.ret.source_name(),
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
    parameters_display_with_type(signature, call_sig, &Ty::source_name)
}

fn parameters_display_with_type(
    signature: &crate::libraries::GenericSig,
    call_sig: &CallSig,
    render: &dyn Fn(Ty) -> String,
) -> Vec<String> {
    signature
        .params
        .iter()
        .enumerate()
        .map(|(index, parameter)| {
            let parameter_name = call_sig
                .param_names
                .get(index)
                .map(String::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("p{index}"));
            let default = if call_sig.param_defaults.get(index).copied().unwrap_or(false) {
                " = ..."
            } else {
                ""
            };
            let (vararg, parameter) = if call_sig.vararg_index == Some(index) {
                ("vararg ", parameter.array_read_elem().unwrap_or(*parameter))
            } else {
                ("", *parameter)
            };
            format!("{vararg}{parameter_name}: {}{default}", render(parameter))
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

/// kotlinc's wording for `expected` type arguments: `no type arguments expected`.
pub(super) fn type_argument_count_wording(expected: usize) -> String {
    match expected {
        0 => "no type arguments expected".to_string(),
        1 => "one type argument expected".to_string(),
        count => format!("{count} type arguments expected"),
    }
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
