//! JVM realization of checked dependency function references.
//!
//! FIR has already selected the declaration and fixed the logical invocation signature. This pass
//! performs one provider-identity lookup to attach JVM owner/name/descriptor facts and synthesizes
//! the runtime `FunctionReferenceImpl` carrier; it does not resolve a source name or select an
//! overload.

use super::classpath::{Classpath, ExternalCallableKind};
use crate::fir::ExternalCallableId;
use crate::ir::{FrDispatch, FuncRef, IrClass, IrExpr, IrFile};
use crate::types::{type_name, Ty};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FunctionReferenceRealizationTarget {
    External(ExternalCallableId),
    Module(crate::fir::CallableId),
    Adapter(crate::ir::FunId),
    Invalid,
}

fn adapted_flags(adaptation: &crate::fir::FirReferenceAdaptation, declaration_result: Ty) -> i32 {
    let vararg_conversion = adaptation.arguments.iter().any(|argument| {
        matches!(
            argument,
            crate::fir::FirAdaptedReferenceArgument::Vararg {
                whole_array: false,
                ..
            }
        )
    });
    let unit_conversion =
        adaptation.result_type.get() == Ty::Unit && declaration_result != Ty::Unit;
    i32::from(vararg_conversion)
        | (i32::from(adaptation.suspend_conversion) << 1)
        | (i32::from(unit_conversion) << 2)
}

/// The class a callable reference at `expression` compiles to: the name the source file's
/// local-class naming walk gives it. A reference that walk never saw (one lowering synthesized, or
/// a second copy an inline splice made) keeps an internal name.
pub(super) fn reference_class_name(
    ir: &IrFile,
    current_facade: &str,
    expression: usize,
    kind: &str,
) -> crate::types::TypeName {
    u32::try_from(expression)
        .ok()
        .and_then(|raw| crate::jvm::local_class_names::callable_reference_name(ir, raw))
        // A second node carrying the same source name is a copy of the first; it cannot share
        // the class.
        .filter(|name| ir.classes.iter().all(|class| class.fq_name != *name))
        .unwrap_or_else(|| type_name(&format!("{current_facade}$fir${kind}${}", ir.classes.len())))
}

/// What a carrier reflects for a dependency target: kotlinc names the declaration's physical
/// owner and JVM signature, except that a member is owned by the classifier it was referenced on.
/// A top-level or extension function is flagged top-level, as its owner is a file facade. A package
/// builtin the compiler implements has no facade: kotlinc reflects it on `Intrinsics.Kotlin`, not
/// top-level, with the JVM signature its declaration maps to.
fn external_reflection(
    classpath: &Classpath,
    declaration: ExternalCallableId,
    receiver: Option<Ty>,
) -> Result<
    (Option<crate::types::TypeName>, String, bool, Option<String>),
    FunctionReferenceRealizationTarget,
> {
    let realization = classpath
        .external_callable(declaration)
        .ok_or(FunctionReferenceRealizationTarget::External(declaration))?;
    let callable = realization.callable;
    let name = callable
        .reflection_name
        .clone()
        .unwrap_or_else(|| callable.name.clone());
    let descriptor = if callable.descriptor.is_empty() {
        // A compiler-implemented declaration has no JVM method; its signature is the one its
        // declaration maps to.
        if callable.compiler_intrinsic.is_none() {
            return Err(FunctionReferenceRealizationTarget::External(declaration));
        }
        let parameters = callable
            .physical_params
            .iter()
            .map(super::ir_emit::ir_ty_to_jvm)
            .collect::<Vec<_>>();
        crate::jvm::names::method_descriptor(
            &parameters,
            super::ir_emit::ir_ty_to_jvm(&callable.physical_ret),
        )
    } else {
        callable.descriptor.clone()
    };
    let owner = callable.owner.render();
    let (owner_class, physical_name, top_level) = match realization.kind {
        ExternalCallableKind::TopLevel | ExternalCallableKind::Extension
            if callable.descriptor.is_empty() =>
        {
            (
                Some(crate::types::wk::kotlin_intrinsics_reflection_owner()),
                callable.name.as_str(),
                false,
            )
        }
        ExternalCallableKind::TopLevel | ExternalCallableKind::Extension => {
            (Some(callable.owner), callable.name.as_str(), true)
        }
        ExternalCallableKind::Member => (
            receiver.and_then(Ty::kotlin_class_internal),
            crate::jvm::names::mapped_builtin_virtual_name(&owner, &callable.name, &descriptor),
            false,
        ),
        // A selected function reference names a function, never a constructor or a field.
        _ => return Err(FunctionReferenceRealizationTarget::External(declaration)),
    };
    let signature = format!("{physical_name}{descriptor}");
    Ok((owner_class, name, top_level, Some(signature)))
}

/// The scope the reference at `expression` is written in, which its class is enclosed by.
pub(super) fn reference_enclosure(
    ir: &IrFile,
    expression: usize,
) -> Option<crate::ir::IrEnclosure> {
    u32::try_from(expression)
        .ok()
        .and_then(|raw| ir.callable_reference_enclosures.get(&raw))
        .copied()
}

fn realize_adapter_reference(
    ir: &mut IrFile,
    classpath: &Classpath,
    current_facade: &str,
    expression: usize,
    adapter_owner: Option<crate::types::TypeName>,
    own_invoke: bool,
    reference: crate::ir::IrCallableReference,
) -> Result<(), FunctionReferenceRealizationTarget> {
    let function = ir
        .functions
        .get(reference.adapter as usize)
        .ok_or(FunctionReferenceRealizationTarget::Adapter(
            reference.adapter,
        ))?
        .clone();
    let Ty::Fun(function_type) = reference.function_type.non_null() else {
        return Err(FunctionReferenceRealizationTarget::Invalid);
    };
    let arity = u8::try_from(function_type.params.len())
        .map_err(|_| FunctionReferenceRealizationTarget::Invalid)?;
    let capture_types = function
        .params
        .get(..reference.captures.len())
        .ok_or(FunctionReferenceRealizationTarget::Adapter(
            reference.adapter,
        ))?
        .to_vec();
    if adapter_owner.is_some() {
        // The carrier is a separate JVM class. A class-owned common adapter therefore crosses a
        // classfile access boundary even though both artifacts represent one Kotlin lexical scope.
        // The adapter has no source declaration or metadata entry, so expose it only physically and
        // mark it synthetic rather than leaking an access-bridge decision into common lowering.
        ir.private_methods.remove(&reference.adapter);
        ir.synthetic_methods.insert(reference.adapter);
    }
    // Converting an ordinary function to a `suspend` function type adapts it even when nothing
    // else about the call changes.
    let suspend_conversion = function_type.suspend && !reference.declaration_suspend;
    let adaptation_flags = reference.adaptation.as_deref().map_or(0, |adaptation| {
        adapted_flags(adaptation, reference.declaration_result)
    }) | (i32::from(suspend_conversion) << 1);
    let reflected = match reference.target {
        crate::ir::IrCallableReferenceTarget::Module(_)
        | crate::ir::IrCallableReferenceTarget::Local { .. } => {
            crate::ir::ReflectedCallable::Source
        }
        crate::ir::IrCallableReferenceTarget::Constructor { .. } => {
            crate::ir::ReflectedCallable::Constructor
        }
        crate::ir::IrCallableReferenceTarget::External { .. } => {
            crate::ir::ReflectedCallable::Physical
        }
    };
    let (owner_class, name, top_level, reflection_signature) = match reference.target {
        crate::ir::IrCallableReferenceTarget::Module(target) => {
            let declaration = ir
                .referenced_module_callables
                .get(&target)
                .ok_or(FunctionReferenceRealizationTarget::Module(target))?;
            (
                declaration.owner,
                declaration.name.to_string(),
                declaration.owner.is_none(),
                None,
            )
        }
        crate::ir::IrCallableReferenceTarget::Constructor { classifier } => {
            (Some(classifier), "<init>".to_string(), false, None)
        }
        crate::ir::IrCallableReferenceTarget::Local { owner, name } => {
            (owner, name.into(), owner.is_none(), None)
        }
        crate::ir::IrCallableReferenceTarget::External {
            declaration,
            receiver,
        } => external_reflection(classpath, declaration, receiver)?,
    };
    let adapted = reference.adaptation.is_some() || suspend_conversion;
    let bound = reference.bound_receiver.is_some();
    let continuation = Ty::obj("kotlin/coroutines/Continuation");
    let mut invoke_parameters = function_type.params.clone();
    let mut target_parameters = function.params.clone();
    let mut invoke_result = function_type.ret;
    let mut target_result = function.ret;
    if function_type.suspend {
        invoke_parameters.push(continuation);
        target_parameters.push(continuation);
        invoke_result = Ty::obj("kotlin/Any");
        target_result = Ty::obj("kotlin/Any");
    }
    let mut reflection_parameters = reference.declaration_parameters.into_vec();
    let mut reflection_result = reference.declaration_result;
    if reference.declaration_suspend {
        reflection_parameters.push(continuation);
        reflection_result = Ty::obj("kotlin/Any");
    }
    let internal = reference_class_name(ir, current_facade, expression, "function");
    let mut class = IrClass::synthetic(internal);
    class.enclosure = reference_enclosure(ir, expression);
    class.superclass = type_name(if adapted {
        "kotlin/jvm/internal/AdaptedFunctionReference"
    } else {
        "kotlin/jvm/internal/FunctionReferenceImpl"
    });
    class.func_ref = Some(FuncRef {
        adapted,
        bound,
        field_capture_count: u32::try_from(reference.captures.len())
            .map_err(|_| FunctionReferenceRealizationTarget::Invalid)?,
        arity,
        is_suspend: function_type.suspend,
        module_target: None,
        local_target: Some(reference.adapter),
        owner_class,
        fn_name: name,
        flags: i32::from(top_level) | (adaptation_flags << 1),
        dispatch: if bound {
            FrDispatch::StaticBound
        } else {
            FrDispatch::Static
        },
        call_owner: adapter_owner,
        call_name: function.name,
        reflection_name: None,
        reflection_receiver_parameter: false,
        reflection_target_ret_ty: Some(reflection_result),
        reflection_target_param_tys: Some(reflection_parameters),
        call_interface: false,
        param_tys: invoke_parameters,
        ret_ty: invoke_result,
        target_param_tys: target_parameters,
        target_ret_ty: target_result,
        unbox_params: vec![None; function_type.params.len()],
        unbox_param_nullable: vec![false; function_type.params.len()],
        box_ret: None,
        staticbound_recv_unbox: None,
        invoke: None,
        function_type: reference.function_type.non_null(),
        reflection_signature,
        reflected,
        invoke_renamed: false,
    });
    let class = ir.add_class(class);
    if own_invoke {
        realize_own_invoke(
            ir,
            expression,
            class,
            reference.adapter,
            bound,
            function_type,
            adapter_owner,
        );
    }
    let mut constructor_arguments = reference.captures;
    constructor_arguments.extend(reference.bound_receiver);
    let carrier = match constructor_arguments.as_slice() {
        arguments if !arguments.is_empty() => {
            let mut constructor_parameters = capture_types;
            if bound {
                constructor_parameters.push(Ty::obj("kotlin/Any"));
            }
            IrExpr::New {
                internal,
                args: arguments.to_vec(),
                ctor_params: Some(constructor_parameters),
                ctor_desc: None,
                external_target: None,
                defaults: Box::new([]),
                default_prefix_count: 0,
            }
        }
        [] => IrExpr::StaticInstance {
            owner: class,
            ty: class,
            field: "INSTANCE",
        },
        _ => unreachable!("empty and non-empty capture shapes are exhaustive"),
    };
    install_carrier(ir, expression, carrier, reference.function_type);
    Ok(())
}

/// Replace the reference expression with its carrier, cast to the reference's function type.
/// kotlinc's `FunctionReferenceLowering` hands the carrier to its use site through that implicit
/// cast, which the JVM writes as a `checkcast` to the `FunctionN` interface.
fn install_carrier(ir: &mut IrFile, expression: usize, carrier: IrExpr, function_type: Ty) {
    let carrier = ir.add_expr(carrier);
    ir.exprs[expression] = IrExpr::TypeOp {
        op: crate::ir::IrTypeOp::Cast,
        arg: carrier,
        type_operand: function_type.non_null(),
    };
}

/// Whether a structural reference's adapter can become the carrier's own `invoke`. A local
/// function's field captures keep the synthesized dispatching `invoke`.
fn own_invoke_realizable(ir: &IrFile, reference: &crate::ir::IrCallableReference) -> bool {
    reference.captures.is_empty()
        && ir
            .functions
            .get(reference.adapter as usize)
            .is_some_and(|adapter| adapter.body.is_some())
}

/// Turn the reference's generated adapter into the carrier's specialized `invoke`, the method
/// kotlinc's `FunctionReferenceLowering` writes: an instance method over the function type's own
/// parameters that returns its result, boxed where it overrides the generic `R`. The adapter's
/// static layout (bound receiver, then parameters) becomes the instance layout: `this` takes value
/// 0, and the bound receiver is read from the inherited `receiver` field and cast to its type.
fn realize_own_invoke(
    ir: &mut IrFile,
    expression: usize,
    class: crate::ir::ClassId,
    adapter: crate::ir::FunId,
    bound: bool,
    function_type: &crate::types::FnSig,
    adapter_owner: Option<crate::types::TypeName>,
) {
    let internal = ir.classes[class as usize].fq_name;
    let Some(body) = ir.functions[adapter as usize].body else {
        return;
    };
    let expressions = crate::ir::value_namespace_expressions(ir, body);
    let receiver_ty = bound.then(|| ir.functions[adapter as usize].params[0]);
    if let Some(receiver_ty) = receiver_ty {
        let field = u32::try_from(ir.classes[class as usize].fields.len())
            .expect("a reference carrier has few fields");
        // The runtime base class stores the bound receiver; the carrier reads it as its own field.
        ir.classes[class as usize].fields.push(crate::ir::IrField {
            name: "receiver".to_string(),
            ty: Ty::nullable(Ty::obj("kotlin/Any")),
            constructor_store_line: 0,
            type_param: None,
            default: None,
            flags: crate::ir::IrfFlags::default(),
        });
        for &current in &expressions {
            if matches!(ir.exprs[current as usize], IrExpr::GetValue(0)) {
                let this = ir.add_expr(IrExpr::GetValue(0));
                let stored = ir.add_expr(IrExpr::GetField {
                    receiver: this,
                    class,
                    index: field,
                });
                ir.exprs[current as usize] = IrExpr::TypeOp {
                    op: crate::ir::IrTypeOp::ImplicitCoercion,
                    arg: stored,
                    type_operand: receiver_ty,
                };
            }
        }
    } else {
        crate::ir::shift_value_indices(ir, body, 0, 1);
    }
    // kotlinc attributes the whole body to the reference's line, entered where the call's own
    // operands begin: at each parameter read, or at a constructed object's `new`. Every other node
    // enters it only at its dispatch, so a bound receiver's read stays ahead of the line.
    if let Some(line) = ir.expr_source_lines.get(&(expression as u32)).copied() {
        // The bridge maps its whole body to the same line.
        ir.fn_decl_lines.insert(adapter, line);
        let parameter_count = function_type.params.len() as u32;
        for &current in &expressions {
            let marks = match &ir.exprs[current as usize] {
                IrExpr::GetValue(value) => (1..=parameter_count).contains(value),
                IrExpr::New { .. } => true,
                _ => false,
            };
            if marks {
                ir.expr_source_lines.insert(current, line);
            } else {
                ir.expr_source_lines.remove(&current);
                ir.mark_dispatch_line(current, line);
            }
        }
    }
    let parameters = function_type.params.clone();
    // A suspend `invoke` keeps its declared result: the suspend lowering that follows appends the
    // continuation and returns the result as an object, boxed as a coroutine boxes it. An unsigned
    // result stays its carrier, which the bridge boxes, as a value class does. Past the numbered
    // interfaces no generic `R` is overridden, so the result stays scalar too.
    let high_arity = parameters.len() > crate::jvm::names::MAX_NUMBERED_FUNCTION_ARITY;
    let result = match function_type.ret {
        ret if ret.is_jvm_scalar()
            && !function_type.suspend
            && !ret.is_unsigned()
            && !high_arity =>
        {
            Ty::nullable(ret)
        }
        ret => ret,
    };
    // The adapter returns the `FunctionN` result value; the specialized `invoke` returns its own
    // declared result instead: nothing for `Unit` (a `void` method), and the boxed value where a
    // scalar result overrides the generic `R`.
    for &current in &expressions {
        let IrExpr::Return(Some(value)) = ir.exprs[current as usize] else {
            continue;
        };
        if result == Ty::Unit && matches!(ir.exprs[value as usize], IrExpr::UnitInstance) {
            ir.exprs[current as usize] = IrExpr::Return(None);
        } else if result != function_type.ret {
            let boxed = ir.add_expr(IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::ImplicitCoercion,
                arg: value,
                type_operand: result,
            });
            ir.exprs[current as usize] = IrExpr::Return(Some(boxed));
        }
    }
    // kotlinc checks no parameter of a suspend reference's `invoke`.
    let param_checks = parameters
        .iter()
        .map(|ty| {
            (!function_type.suspend && ty.is_reference() && !ty.upper_bound_admits_null())
                .then_some(crate::ir::IrParameterCheck::NonNull)
        })
        .collect();
    // kotlinc's reference lowering declares the `invoke` parameters itself.
    let identities = (0..parameters.len())
        .map(|ordinal| {
            crate::ir::IrParameterIdentity::generated(
                crate::ir::IrGeneratedParameterRole::ReferenceInvokeValue {
                    ordinal: u32::try_from(ordinal).expect("too many reference parameters"),
                },
                None,
            )
        })
        .collect();
    ir.fn_params
        .insert(adapter, crate::ir::FnParamInfo::identities(identities));
    let function = &mut ir.functions[adapter as usize];
    function.name = "invoke".to_string();
    function.params = parameters;
    function.ret = result;
    function.is_static = false;
    function.dispatch_receiver = Some(internal);
    function.param_checks = param_checks;
    ir.private_methods.remove(&adapter);
    ir.synthetic_methods.remove(&adapter);
    ir.class_static_local_functions.remove(&adapter);
    ir.lambda_own_params_from.remove(&adapter);
    ir.fn_debug_locals.insert(adapter);
    // kotlinc writes no nullability annotations on a reference carrier's `invoke`.
    ir.jvm_nullability_unannotated_methods.insert(adapter);
    if let Some(owner) = adapter_owner {
        if let Some(owner) = ir
            .classes
            .iter_mut()
            .find(|candidate| candidate.fq_name == owner)
        {
            owner.methods.retain(|&method| method != adapter);
        }
    }
    let carrier = &mut ir.classes[class as usize];
    carrier.methods.push(adapter);
    let reference = carrier
        .func_ref
        .as_mut()
        .expect("the carrier was just built as a function reference");
    reference.local_target = None;
    reference.invoke = Some(adapter);
}

pub(super) fn realize(
    ir: &mut IrFile,
    classpath: &Classpath,
    current_facade: &str,
) -> Result<(), FunctionReferenceRealizationTarget> {
    let adapter_owners = ir
        .classes
        .iter()
        .flat_map(|class| {
            class
                .methods
                .iter()
                .copied()
                .map(|function| (function, class.fq_name_id()))
        })
        .collect::<std::collections::HashMap<_, _>>();
    // An adapter several reference nodes share (an inline splice copies the node, not its
    // adapter) stays one static method every carrier calls; only a sole reference owns it.
    let mut adapter_uses = std::collections::HashMap::<crate::ir::FunId, usize>::new();
    for expression in &ir.exprs {
        if let IrExpr::CallableReference(reference) = expression {
            *adapter_uses.entry(reference.adapter).or_default() += 1;
        }
    }
    let expression_count = ir.exprs.len();
    for raw in 0..expression_count {
        let IrExpr::CallableReference(reference) = ir.exprs[raw].clone() else {
            continue;
        };
        let adapter_owner = adapter_owners.get(&reference.adapter).copied();
        let sole = adapter_uses.get(&reference.adapter) == Some(&1);
        let own_invoke = sole && own_invoke_realizable(ir, &reference);
        realize_adapter_reference(
            ir,
            classpath,
            current_facade,
            raw,
            adapter_owner,
            own_invoke,
            reference,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn own_invoke_plan(function_type: Ty, declaration_suspend: bool, captured: bool) -> bool {
        let mut ir = IrFile::default();
        let body = ir.add_expr(IrExpr::UnitInstance);
        let Ty::Fun(signature) = function_type.non_null() else {
            unreachable!("test function type")
        };
        let adapter = ir.add_fun(crate::ir::IrFunction {
            name: "selected".to_string(),
            params: signature.params.clone(),
            ret: signature.ret,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let capture = captured.then(|| ir.add_expr(IrExpr::UnitInstance));
        let reference = crate::ir::IrCallableReference {
            target: crate::ir::IrCallableReferenceTarget::Local {
                owner: None,
                name: "selected".into(),
            },
            adapter,
            captures: capture.into_iter().collect(),
            bound_receiver: None,
            function_type,
            declaration_parameters: signature.params.clone().into_boxed_slice(),
            declaration_result: signature.ret,
            declaration_suspend,
            adaptation: None,
        };
        own_invoke_realizable(&ir, &reference)
    }

    #[test]
    fn own_invoke_plan_excludes_only_unrealized_carrier_shapes() {
        let plans = [
            (
                "ordinary",
                own_invoke_plan(Ty::fun(vec![Ty::Int], Ty::Int), false, false),
            ),
            (
                "captured",
                own_invoke_plan(Ty::fun(vec![Ty::String], Ty::String), false, true),
            ),
            (
                "suspend",
                own_invoke_plan(Ty::fun_suspend(vec![Ty::Int], Ty::Int), true, false),
            ),
            (
                "high arity",
                own_invoke_plan(Ty::fun(vec![Ty::Int; 23], Ty::Int), false, false),
            ),
            (
                "value class",
                own_invoke_plan(Ty::fun(vec![Ty::UInt], Ty::String), false, false),
            ),
        ];

        assert_eq!(
            plans,
            [
                ("ordinary", true),
                ("captured", false),
                ("suspend", true),
                ("high arity", true),
                ("value class", true),
            ]
        );
    }

    #[test]
    fn adapter_body_walk_keeps_nested_lambda_value_and_return_namespaces_separate() {
        let mut ir = IrFile::default();
        let capture = ir.add_expr(IrExpr::GetValue(2));
        let nested_value = ir.add_expr(IrExpr::GetValue(0));
        let nested_return = ir.add_expr(IrExpr::Return(Some(nested_value)));
        let nested_body = ir.add_expr(IrExpr::Block {
            stmts: vec![nested_return],
            value: None,
        });
        let lambda = ir.add_expr(IrExpr::Lambda {
            impl_fn: 0,
            arity: 1,
            captures: vec![capture],
            sam: None,
            inline_body: Some(nested_body),
        });
        let outer_value = ir.add_expr(IrExpr::GetValue(0));
        let outer_return = ir.add_expr(IrExpr::Return(Some(outer_value)));
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![lambda, outer_return],
            value: None,
        });

        let expressions = crate::ir::value_namespace_expressions(&ir, body);

        for expected in [body, lambda, capture, outer_return, outer_value] {
            assert!(
                expressions.contains(&expected),
                "missing outer expression {expected}"
            );
        }
        for nested in [nested_body, nested_return, nested_value] {
            assert!(
                !expressions.contains(&nested),
                "nested lambda expression {nested} crossed its value namespace"
            );
        }
    }
}
