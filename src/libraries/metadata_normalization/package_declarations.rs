//! Package-level Kotlin declarations normalized into common library candidates.

use super::parameter_identities::{FunctionParameterIdentities, PropertyParameterIdentities};
use super::type_signatures::{
    function_generic_sig, only_input_type_formals, property_generic_sig,
    reified_type_parameter_ordinals,
};
use crate::libraries::{
    CallSig, FnFlags, FnKind, FunctionInfo, InlineKind, LibraryCallable, LibraryConst, PropKind,
    PropertyInfo, PropertyProducer, PropertyReadStability,
};
use crate::metadata::semantic::{KotlinFunction, KotlinProperty};
use crate::types::{stored_value_ty, SemanticCallableOwner, Ty, TypeName};

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

/// The declared names of a property's accessors, as the publishing artifact names them.
pub(crate) struct PropertyAccessorNames<'a> {
    pub(crate) getter: &'a str,
    /// Present exactly for a `var`.
    pub(crate) setter: Option<&'a str>,
}

/// Normalize one top-level property of `package` into a selection candidate.
///
/// The property type and every accessor parameter stay semantic. Each accessor lists its
/// parameters in physical source order (leading context parameters, then the extension receiver,
/// then a setter's value); its result is the property type, or `Unit` for a setter. The candidate
/// names no physical owner, descriptor, or storage and carries no provider identity; the provider
/// that publishes it assigns one to each accessor and to the property.
///
/// `parameters` are the property's validated parameter identities. As for functions,
/// normalization attaches no compiler intrinsic and no semantic role to either accessor.
pub(crate) fn package_property(
    package: TypeName,
    property: &KotlinProperty,
    parameters: &PropertyParameterIdentities,
    accessors: PropertyAccessorNames<'_>,
) -> PropertyInfo {
    let generic_sig = property_generic_sig(property);
    let receiver = generic_sig.receiver;
    let ty = generic_sig.ret;
    let getter_params: Vec<Ty> = generic_sig
        .params
        .iter()
        .chain(&receiver)
        .copied()
        .collect();
    let accessor = |name: &str, params: Vec<Ty>, ret: Ty| {
        let mut callable = LibraryCallable::library(package, name, params, ret, ret, String::new());
        callable.declaration_owner = Some(SemanticCallableOwner::Package(package));
        callable.source_receiver = receiver;
        callable.context_count = property.context_count;
        callable
    };

    let mut getter = accessor(accessors.getter, getter_params.clone(), ty);
    getter.visibility = property.visibility;
    getter.annotations = property.annotations.clone();
    let mut setter = accessors.setter.map(|name| {
        let params = getter_params
            .iter()
            .copied()
            .chain([stored_value_ty(ty)])
            .collect();
        let mut setter = accessor(name, params, Ty::Unit);
        setter.visibility = property.setter_visibility;
        setter
    });
    if !generic_sig.formals.is_empty() {
        if let Some(setter) = &mut setter {
            let mut setter_sig = generic_sig.clone();
            setter_sig.params.push(ty);
            setter_sig.ret = Ty::Unit;
            setter.generic_sig = Some(Box::new(setter_sig));
        }
        getter.generic_sig = Some(Box::new(generic_sig.clone()));
    }

    let compile_time_constant = property
        .constant
        .clone()
        .filter(|_| property.is_const)
        .map(|value| LibraryConst { ty, value });
    PropertyInfo {
        name: property.name.clone(),
        kind: if receiver.is_some() {
            PropKind::Extension
        } else {
            PropKind::TopLevel
        },
        receiver,
        associated_classifier: None,
        associated_access_owner: None,
        formals: generic_sig.formals,
        ty,
        context_count: property.context_count,
        context_param_names: property.context_param_names.clone(),
        context_parameter_identities: parameters.contexts.clone(),
        getter,
        setter,
        setter_visibility: property.setter_visibility,
        setter_parameter_name: property.setter_parameter_name.clone(),
        is_const: property.is_const,
        implicit_integer_coercion: property
            .annotations
            .iter()
            .any(|annotation| *annotation == crate::types::wk::implicit_integer_coercion()),
        compile_time_constant,
        metadata_constant_read: false,
        visibility: property.visibility,
        owner: package,
        receiver_rank: 0,
        source_key: None,
        stable_declaration: None,
        getter_declaration: None,
        setter_declaration: None,
        source_member: None,
        producer: PropertyProducer::KotlinAccessor,
        // A dependency property's reads are never stable for smart casts.
        read_stability: PropertyReadStability::Unstable,
        return_value_status: None,
    }
}
