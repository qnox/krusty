//! JVM realization of Kotlin non-null parameter contracts.
//!
//! Checked FIR/common IR retain semantic parameter types and source names. Whether those contracts
//! become `Intrinsics.checkNotNullParameter` calls is a JVM backend choice, made here before type
//! erasure. Value-class lowering may subsequently remove a guard when the selected carrier is a
//! primitive. No frontend phase records an intrinsic name or makes a JVM representation decision.

use crate::ir::{FunId, IrFile, IrLambdaForm, IrParameterRole};
use crate::types::Ty;
use std::collections::HashSet;

/// kotlinc guards a parameter whose type is non-null and whose JVM type is not primitive, which
/// includes `Unit` (`kotlin.Unit`) and `Nothing` (`java.lang.Void`).
pub(super) fn requires_reference_guard(ty: Ty) -> bool {
    (ty.is_reference() || ty == Ty::Unit || ty == Ty::Nothing) && !ty.upper_bound_admits_null()
}

fn realize_function(ir: &mut IrFile, function: FunId) {
    if ir.method_visibility(function).is_private() {
        return;
    }
    let Some(parameters) = ir
        .functions
        .get(function as usize)
        .map(|value| value.params.clone())
    else {
        return;
    };
    let declared_nullable = ir
        .fn_param_declared_nullable
        .get(&function)
        .cloned()
        .unwrap_or_default();
    // A guard quotes the parameter's name. A value parameter whose provider publishes none (a
    // Java-declared parameter of a mapped collection interface, forwarded by delegation) has no
    // spelling to quote, and a name is never invented for it.
    let unnamed = ir
        .function_parameter_identities(function)
        .map(|identities| {
            identities
                .iter()
                .map(|identity| {
                    identity.role == IrParameterRole::Value && identity.source_name.is_none()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let checks = &mut ir.functions[function as usize].param_checks;
    checks.resize(parameters.len(), None);
    for (ordinal, ty) in parameters.into_iter().enumerate() {
        if checks[ordinal].is_some()
            || declared_nullable.get(ordinal).copied().unwrap_or(false)
            || unnamed.get(ordinal).copied().unwrap_or(false)
            || !requires_reference_guard(ty)
        {
            continue;
        }
        checks[ordinal] = Some(crate::ir::IrParameterCheck::NonNull);
    }
}

/// kotlinc's `generateNonNullAssertions` skips a private function unless it is the local function
/// of a lambda literal (`LOCAL_FUNCTION_FOR_LAMBDA`): that one guards its own receiver, the
/// anonymous context parameters of the function type it is checked against, and its value
/// parameters, a bare `_` or destructuring pattern included, but not the values it captures. An
/// anonymous function (`fun(…) {}`) lowers to an ordinary private local function and guards
/// nothing, and a suspend lambda becomes a class whose resumption arguments are null.
fn realize_lambda(ir: &mut IrFile, function: FunId) {
    if ir.suspend_funs.contains(&function) {
        return;
    }
    let Some(identities) = ir.function_parameter_identities(function) else {
        return;
    };
    let guarded = identities
        .iter()
        .map(|identity| match identity.role {
            IrParameterRole::ExtensionReceiver
            | IrParameterRole::AnonymousContextParameter { .. }
            | IrParameterRole::UnusedValue
            | IrParameterRole::DestructuredValue => true,
            // The role decides the guard; the source name is only the message it quotes.
            IrParameterRole::Value => {
                assert!(
                    identity.source_name.is_some(),
                    "a lambda's value parameter carries its source identity"
                );
                true
            }
            _ => false,
        })
        .collect::<Vec<_>>();
    let lambda = &mut ir.functions[function as usize];
    lambda.param_checks.resize(lambda.params.len(), None);
    for ((check, ty), guarded) in lambda
        .param_checks
        .iter_mut()
        .zip(&lambda.params)
        .zip(guarded)
    {
        if guarded && requires_reference_guard(*ty) {
            *check = Some(crate::ir::IrParameterCheck::NonNull);
        }
    }
}

pub(super) fn realize(ir: &mut IrFile) {
    let mut lambdas = ir
        .lambda_origins
        .iter()
        .filter(|(_, origin)| origin.form == IrLambdaForm::Literal)
        .map(|(function, _)| *function)
        .collect::<Vec<_>>();
    lambdas.sort_unstable();
    for function in lambdas {
        realize_lambda(ir, function);
    }
    let mut functions = ir
        .checked_callable_functions
        .values()
        .copied()
        .collect::<HashSet<_>>();
    // Property accessors are source declarations materialized after ordinary callable lowering.
    // Consume their exact common-IR identity edges rather than recovering accessor shape from a
    // JVM method name. This also covers context and extension-receiver parameters.
    for layout in ir.local_property_layouts.values() {
        let (getter, setter) = match layout {
            crate::ir::IrLocalPropertyLayout::TopLevelStorage { getter, setter, .. }
            | crate::ir::IrLocalPropertyLayout::Member { getter, setter, .. } => (*getter, *setter),
            crate::ir::IrLocalPropertyLayout::TopLevelAccessor { getter, setter, .. }
            | crate::ir::IrLocalPropertyLayout::MemberExtension { getter, setter, .. } => {
                (Some(*getter), *setter)
            }
        };
        functions.extend(getter);
        functions.extend(setter);
    }
    // Interface-delegation forwarders are compiler-generated but public, and kotlinc checks their
    // parameters like any source override's. Their exact identities are the override edges they
    // publish.
    for edges in ir.function_overrides.values() {
        functions.extend(edges.iter().filter_map(|edge| edge.implementation_function));
    }
    for edges in ir.property_overrides.values() {
        for edge in edges {
            functions.extend(edge.implementation_getter);
            functions.extend(edge.implementation_setter);
        }
    }
    for function in functions {
        realize_function(ir, function);
    }

    for class in &mut ir.classes {
        // kotlinc guards no private function's parameters: a declared private primary, and a
        // sealed class's, which is private in the class file.
        let private = ir.ctor_visibilities.get(&class.fq_name_id())
            == Some(&crate::types::Visibility::Private);
        if !class.is_source_declared || class.is_sealed || private {
            continue;
        }
        let assertion_names = crate::jvm::parameter_names::constructor_assertions(&class.ctor_args);
        for (parameter, assertion_name) in class.ctor_args.iter_mut().zip(assertion_names) {
            if parameter.check.is_some() {
                continue;
            }
            let Some(name) = assertion_name else {
                continue;
            };
            let Some(declared) = parameter.declared_ty else {
                continue;
            };
            if requires_reference_guard(declared) {
                parameter.check = Some(name);
            }
        }
    }
    realize_secondary_constructors(ir);
}

/// kotlinc guards a declared secondary constructor's non-null reference parameters like a
/// function's, unless no source caller outside the class can reach it: a private one, an enum's, a
/// sealed class's (private in the class file) and one the compiler generated.
fn realize_secondary_constructors(ir: &mut IrFile) {
    let generated = ir
        .classes
        .iter()
        .map(|class| {
            (0..class.secondary_ctors.len())
                .map(|ordinal| {
                    ir.is_generated_secondary_constructor(class.fq_name_id(), ordinal as u32)
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    for (class, generated) in ir.classes.iter_mut().zip(generated) {
        if !class.is_source_declared || class.is_sealed || class.is_enum || class.is_value {
            continue;
        }
        for (constructor, generated) in class.secondary_ctors.iter_mut().zip(generated) {
            if generated
                || constructor.synthetic
                || constructor.metadata_visibility == Some(crate::types::Visibility::Private)
            {
                continue;
            }
            constructor.param_checks = constructor
                .named_params
                .iter()
                .map(|(_, ty)| {
                    requires_reference_guard(*ty).then_some(crate::ir::IrParameterCheck::NonNull)
                })
                .collect();
        }
    }
}

/// Finish secondary-constructor guards after value-class lowering has selected the physical ABI.
/// A constructor with a value-class parameter is private and marker-disambiguated in the class
/// file, so no external JVM caller can reach it and kotlinc emits no parameter guards.
pub(super) fn finalize_after_value_class_lowering(ir: &mut IrFile) {
    for class in &mut ir.classes {
        for constructor in &mut class.secondary_ctors {
            if constructor.vc_params {
                constructor.param_checks.clear();
            }
        }
    }
}

/// Drop every `Intrinsics.checkNotNullParameter` guard the lowering recorded.
///
/// `-Xno-param-assertions` removes the parameter null checks kotlinc emits at the entry of every
/// function reachable from Java. Applied to the IR rather than at the emission site on purpose: the
/// guards are also what the `LineNumberTable` and `LocalVariableTable` start offsets are computed
/// from, so suppressing them at one site and not the other would emit debug tables pointing into the
/// middle of the method.
pub(super) fn strip(ir: &mut IrFile) {
    for function in &mut ir.functions {
        function.param_checks.fill(None);
    }
    for class in &mut ir.classes {
        for parameter in &mut class.ctor_args {
            parameter.check = None;
        }
        for constructor in &mut class.secondary_ctors {
            constructor.param_checks.clear();
        }
    }
}
