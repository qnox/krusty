//! The calls a generated forwarder makes on the delegate value's own static type.
//!
//! kotlinc's forwarder for `class L : List<String> by ArrayList<String>()` stores the delegate in a
//! field of the delegate expression's type and calls the member of that type which overrides the
//! delegated declaration: `ArrayList<String>.get`, realized on `ArrayList`. The forwarder's
//! declared result is the delegated declaration's, so kotlinc's implicit not-null cast
//! (`Fir2IrImplicitCastInserter.insertSpecialCast`) checks a delegate result that is flexible or
//! enhanced to not-null whenever that declared result rejects `null`.
//!
//! The Pass-1 forwarding plan is selected from the interface before any body is checked; this
//! module selects the delegate-side calls once the checker knows the delegate value's type. It uses
//! the same override slots, so each forwarder's delegate call overrides the declaration it
//! forwards.

use crate::fir::{
    ResolvedDelegateCall, ResolvedDelegateMemberCalls, ResolvedDelegatedFunction,
    ResolvedDelegatedMember, ResolvedDelegatedProperty, ResolvedInterfaceDelegation,
    ResolvedModuleIndex,
};
use crate::libraries::{FnKind, ResultEnhancement};
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

use super::{
    applied_owner_type, applied_property_signature, effective_function, effective_property,
    function_call, interface_owners, is_delegated_function, property_call, PropertyCallShape,
};

/// Select, for every forwarder of `delegation`, the call it makes on a delegate of type
/// `delegate`.
pub(in crate::resolve) fn delegate_member_calls(
    source: &dyn SymbolSource,
    index: &ResolvedModuleIndex,
    delegation: &ResolvedInterfaceDelegation,
    delegate: Ty,
) -> Option<Box<[ResolvedDelegateMemberCalls]>> {
    let interface = delegation.interface.get();
    // A type-parameter delegate offers its bound's members.
    let mut delegate = delegate.non_null();
    while let Ty::TyParam(_, bound) = delegate {
        delegate = bound.non_null();
    }
    if !matches!(delegate, Ty::Obj(..)) {
        return Some(
            delegation
                .members
                .iter()
                .map(|member| match member {
                    ResolvedDelegatedMember::Function(function) => {
                        ResolvedDelegateMemberCalls::Function(unchanged(&function.call))
                    }
                    ResolvedDelegatedMember::Property(property) => unchanged_property(property),
                })
                .collect(),
        );
    }
    delegation
        .members
        .iter()
        .map(|member| {
            let calls = match member {
                ResolvedDelegatedMember::Function(function) => {
                    delegate_function_call(source, index, interface, delegate, function)
                        .map(ResolvedDelegateMemberCalls::Function)
                }
                ResolvedDelegatedMember::Property(property) => {
                    delegate_property_calls(source, index, delegate, property)
                }
            };
            crate::trace_compiler!(
                "resolve",
                "delegate member calls delegate={delegate:?} member={member:?} calls={calls:?}",
            );
            calls
        })
        .collect()
}

/// The interface declaration's own call, unchecked: what a delegate offering no more specific
/// member forwards to.
fn unchanged(call: &crate::fir::ResolvedDelegatedCall) -> ResolvedDelegateCall {
    ResolvedDelegateCall {
        call: call.clone(),
        result_check: None,
    }
}

fn unchanged_property(property: &ResolvedDelegatedProperty) -> ResolvedDelegateMemberCalls {
    ResolvedDelegateMemberCalls::Property {
        getter: unchanged(&property.getter),
        setter: property.setter.as_ref().map(unchanged),
    }
}

/// Whether `implementation`, found on the delegate, overrides the forwarded `declaration`: the same
/// input shape, a Java parameter's flexibility included.
fn overrides(
    source: &dyn SymbolSource,
    implementation: &crate::libraries::FunctionInfo,
    declaration: &crate::libraries::FunctionInfo,
) -> bool {
    let formals = |function: &crate::libraries::FunctionInfo| {
        function
            .generic_sig
            .as_ref()
            .map(|signature| signature.formals.clone())
            .unwrap_or_default()
    };
    implementation.context_count == declaration.context_count
        && implementation.flags.suspend == declaration.flags.suspend
        && crate::symbol_resolver::override_parameter_types_match(
            source,
            &declaration.semantic_params(),
            &formals(declaration),
            &implementation.semantic_params(),
            &formals(implementation),
        )
}

/// Whether two hierarchy views of a member name one declaration.
fn same_declaration(
    function: &crate::libraries::FunctionInfo,
    declaration: &crate::libraries::FunctionInfo,
) -> bool {
    function.callable.owner == declaration.callable.owner
        && function.stable_declaration == declaration.stable_declaration
        && function.callable.external_identity == declaration.callable.external_identity
}

/// kotlinc's `acceptsNullValues` for a forwarder's declared result: a nullable type or a type
/// parameter whose bound admits `null` accepts it.
fn rejects_null(result: Ty) -> bool {
    !result.admits_null()
        && !result.upper_bound_admits_null()
        && (result.is_reference() || result.is_jvm_scalar())
}

fn delegate_function_call(
    source: &dyn SymbolSource,
    index: &ResolvedModuleIndex,
    interface: Ty,
    delegate: Ty,
    forwarded: &ResolvedDelegatedFunction,
) -> Option<ResolvedDelegateCall> {
    // A member extension forwards the interface's own declaration.
    if forwarded.call.extension_receiver_parameter.is_some() {
        return Some(unchanged(&forwarded.call));
    }
    let interface_owners = interface_owners(source, interface)?;
    let declared = crate::symbol_resolver::members_in_hierarchy(source, interface, &forwarded.name);
    let forwarded_declaration = declared
        .functions()
        .iter()
        .filter(|function| is_delegated_function(function, &interface_owners))
        .find(|function| {
            function_call(source, index, interface, function).as_ref() == Some(&forwarded.call)
        })?;
    // kotlinc's `findDelegateToSymbol` relates declarations, not the interface's type arguments: a
    // delegate may implement the interface with other arguments (`InOutBase<D, D>` delegating
    // `InOutBase<D, A>`). The forwarded declaration is therefore compared as the delegate's own
    // supertype applies it, substituted exactly as the delegate's members are.
    let applied_interface =
        applied_owner_type(source, delegate, interface.kotlin_class_internal()?)?;
    let applied_declarations =
        crate::symbol_resolver::members_in_hierarchy(source, applied_interface, &forwarded.name);
    let forwarded_declaration = applied_declarations
        .functions()
        .iter()
        .find(|function| same_declaration(function, forwarded_declaration))?;
    let available = crate::symbol_resolver::members_in_hierarchy(source, delegate, &forwarded.name);
    let candidates = available.functions().iter().filter(|function| {
        function.kind == FnKind::Member && overrides(source, function, forwarded_declaration)
    });
    let selected = effective_function(source, index, delegate, candidates)?;
    let call = function_call(source, index, delegate, selected)?;
    let unchecked = selected.call_sig.result_enhancement == ResultEnhancement::NotNull
        || matches!(
            selected.ret.apply(selected.callable.ret),
            Ty::PlatformNullable(_)
        );
    let result_check = (unchecked && rejects_null(forwarded.call.result.get()))
        .then(|| format!("{}(...)", forwarded.name).into_boxed_str());
    Some(ResolvedDelegateCall { call, result_check })
}

fn delegate_property_calls(
    source: &dyn SymbolSource,
    index: &ResolvedModuleIndex,
    delegate: Ty,
    forwarded: &ResolvedDelegatedProperty,
) -> Option<ResolvedDelegateMemberCalls> {
    // A member-extension property forwards the interface's own accessors.
    if forwarded.getter.extension_receiver_parameter.is_some() {
        return Some(unchanged_property(forwarded));
    }
    let available = crate::symbol_resolver::members_in_hierarchy(source, delegate, &forwarded.name);
    let candidates = available
        .properties()
        .iter()
        .filter(|property| property.kind == crate::libraries::PropKind::Member);
    let (getter, setter) = effective_property(source, index, delegate, candidates)?;
    let (_, property_type) = applied_property_signature(source, index, delegate, getter)?;
    let context_parameters = forwarded
        .context_parameters
        .iter()
        .map(|parameter| parameter.ty.get())
        .collect::<Vec<_>>();
    let shape = |setter| PropertyCallShape {
        setter,
        context_parameters: &context_parameters,
        extension_receiver: None,
        property_type,
    };
    let result_check = (matches!(property_type, Ty::PlatformNullable(_))
        && rejects_null(forwarded.ty.get()))
    .then(|| {
        getter
            .producer
            .platform_check_name(&getter.name, Some(getter.getter.name.as_str()))
            .into_boxed_str()
    });
    let getter_call = ResolvedDelegateCall {
        call: property_call(index, delegate, getter, shape(false))?,
        result_check,
    };
    let setter_call = match (&forwarded.setter, setter) {
        (None, _) => None,
        (Some(_), Some(setter)) => Some(ResolvedDelegateCall {
            call: property_call(index, delegate, setter, shape(true))?,
            result_check: None,
        }),
        (Some(_), None) => return None,
    };
    Some(ResolvedDelegateMemberCalls::Property {
        getter: getter_call,
        setter: setter_call,
    })
}
