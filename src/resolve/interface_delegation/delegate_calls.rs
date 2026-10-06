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
//! module selects the delegate-side calls once the checker knows the delegate value's type. It
//! starts from the stable declaration identity the plan recorded and selects the delegate member
//! that overrides it under the shared override input-shape relation
//! ([`crate::symbol_resolver::function_input_shapes_match`]), so a same-named member of another
//! slot (`fun <T : Any> f(t: T)` beside `fun <T : CharSequence> f(t: T)`) is never a candidate.

use crate::fir::{
    ResolvedDelegateCall, ResolvedDelegateMemberCalls, ResolvedDelegatedCallTarget,
    ResolvedDelegatedFunction, ResolvedDelegatedMember, ResolvedDelegatedModuleTarget,
    ResolvedDelegatedProperty, ResolvedFunctionOverrideTarget, ResolvedInterfaceDelegation,
    ResolvedModuleIndex,
};
use crate::libraries::{FnKind, FunctionInfo, ResultEnhancement};
use crate::resolve::override_plans;
use crate::resolve::platform_value_narrowing::expected_type_rejects_null;
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

use super::{
    applied_owner_type, applied_property_signature, effective_function, effective_property,
    function_call, property_call, PropertyCallShape,
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

/// The forwarded declaration as the delegate's own supertype applies it, selected by the stable
/// identity the forwarding plan recorded.
///
/// kotlinc's `findDelegateToSymbol` relates declarations, not the interface's type arguments: a
/// delegate may implement the interface with other arguments (`InOutBase<D, D>` delegating
/// `InOutBase<D, A>`), so the declaration is read through the delegated interface as the delegate's
/// own hierarchy applies it, substituted exactly as the delegate's members are. Member lookup only
/// enumerates that view; the declaration is the one carrying the plan's identity.
fn forwarded_declaration(
    source: &dyn SymbolSource,
    index: &ResolvedModuleIndex,
    interface: Ty,
    delegate: Ty,
    forwarded: &ResolvedDelegatedFunction,
) -> Option<FunctionInfo> {
    // The plan's first obligation is the declaration its call targets.
    let declaration = forwarded.overridden.first()?;
    let called = match &forwarded.call.target {
        ResolvedDelegatedCallTarget::Module {
            target: ResolvedDelegatedModuleTarget::Function(callable),
            ..
        } => ResolvedFunctionOverrideTarget::Module(*callable),
        ResolvedDelegatedCallTarget::External(identity) => {
            ResolvedFunctionOverrideTarget::External(*identity)
        }
        ResolvedDelegatedCallTarget::Module { .. } => return None,
    };
    if declaration.target != called {
        return None;
    }
    let applied_interface =
        applied_owner_type(source, delegate, interface.kotlin_class_internal()?)?;
    let declared =
        crate::symbol_resolver::members_in_hierarchy(source, applied_interface, &forwarded.name);
    let function = declared.functions().iter().find(|function| {
        override_plans::function_target(index, function) == Some(declaration.target)
    })?;
    // The plan recorded the declaration's own type parameters; a view that disagrees has lost
    // them, and comparing it as non-generic would admit another declaration's override.
    let formals = function
        .generic_sig
        .as_ref()
        .map_or(0, |signature| signature.formals.len());
    (formals == forwarded.type_parameters.len()).then(|| function.clone())
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
    let declaration = forwarded_declaration(source, index, interface, delegate, forwarded)?;
    let available = crate::symbol_resolver::members_in_hierarchy(source, delegate, &forwarded.name);
    let candidates = available.functions().iter().filter(|function| {
        function.kind == FnKind::Member
            && crate::symbol_resolver::function_input_shapes_match(source, &declaration, function)
    });
    let selected = effective_function(source, index, delegate, candidates)?;
    let call = function_call(source, index, delegate, selected)?;
    let unchecked = selected.call_sig.result_enhancement == ResultEnhancement::NotNull
        || matches!(
            selected.ret.apply(selected.callable.ret),
            Ty::PlatformNullable(_)
        );
    let result_check = (unchecked && expected_type_rejects_null(forwarded.call.result.get()))
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
    // A Java read is flexible, or enhanced from the builtin property it realizes.
    let unchecked = matches!(property_type, Ty::PlatformNullable(_))
        || getter.getter.enhanced_result.marks.head();
    let result_check = (unchecked && expected_type_rejects_null(forwarded.ty.get())).then(|| {
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
