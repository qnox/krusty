//! Package-level Kotlin declarations normalized into common library candidates.

use super::parameter_identities::FunctionParameterIdentities;
use super::type_signatures::{
    function_generic_sig, only_input_type_formals, reified_type_parameter_ordinals,
};
use crate::libraries::{CallSig, FnFlags, FnKind, FunctionInfo, InlineKind, LibraryCallable};
use crate::metadata::semantic::KotlinFunction;
use crate::types::{SemanticCallableOwner, TypeName};

/// Normalize one top-level function of `package` into a selection candidate.
///
/// Every type stays semantic: the callable's parameters are the declared ones in physical source
/// order (leading context parameters, then the extension receiver, then value parameters), and its
/// result is the declared result. The candidate names no physical owner or descriptor and carries
/// no provider identity; the provider that publishes it assigns one.
///
/// `parameters` are the function's validated parameter identities, which also prove that its
/// context parameters are a prefix of its parameters.
///
/// Normalization attaches no target realization: a compiler intrinsic or a semantic role is a
/// fact a provider decides from what its artifact says about the declaration's implementation.
pub(crate) fn package_function(
    package: TypeName,
    function: &KotlinFunction,
    parameters: &FunctionParameterIdentities,
) -> FunctionInfo {
    let generic_sig = function_generic_sig(function);
    let (contexts, values) = generic_sig
        .params
        .split_at_checked(function.context_count)
        .expect("validated declarations list their context parameters first");
    let params = contexts
        .iter()
        .chain(&generic_sig.receiver)
        .chain(values)
        .copied()
        .collect();
    let kind = if generic_sig.receiver.is_some() {
        FnKind::Extension
    } else {
        FnKind::TopLevel
    };
    let inline = InlineKind::from_flags(function.is_inline, function.is_inline);
    let reified = reified_type_parameter_ordinals(&function.formals);

    let mut callable = LibraryCallable::library(
        package,
        function.name.clone(),
        params,
        generic_sig.ret,
        generic_sig.ret,
        String::new(),
    );
    callable.declaration_owner = Some(SemanticCallableOwner::Package(package));
    callable.visibility = function.visibility;
    callable.inline = inline;
    callable.suspend = function.is_suspend;
    callable.source_receiver = generic_sig.receiver;
    callable.generic_sig = Some(Box::new(generic_sig.clone()));
    callable.reified_type_parameter_ordinals = reified.clone().into_boxed_slice();
    callable.context_count = function.context_count;
    callable.annotations = function.annotations.clone();

    let mut normalized = FunctionInfo::plain(kind, generic_sig.receiver, callable);
    normalized.call_sig = CallSig::metadata_member(
        generic_sig.params.len(),
        function.param_names.clone(),
        function.param_defaults.clone(),
        function.vararg,
    );
    normalized.call_sig.parameter_identities = parameters.arguments.clone();
    normalized.call_sig.only_input_type_formals = only_input_type_formals(&function.formals);
    normalized.call_sig.reified_type_parameter_ordinals = reified;
    normalized.generic_sig = Some(generic_sig);
    normalized.context_count = function.context_count;
    normalized.visibility = function.visibility;
    normalized.flags = FnFlags {
        inline,
        reified: function.has_reified_type_params,
        suspend: function.is_suspend,
        operator: function.is_operator,
        infix: function.is_infix,
        is_abstract: false,
        is_final: true,
        inherited_by_delegation: false,
        return_value_status: None,
    };
    normalized
}
