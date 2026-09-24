//! JVM realization of checked dependency function references.
//!
//! FIR has already selected the declaration and fixed the logical invocation signature. This pass
//! performs one provider-identity lookup to attach JVM owner/name/descriptor facts and synthesizes
//! the runtime `FunctionReferenceImpl` carrier; it does not resolve a source name or select an
//! overload.

use super::classpath::{Classpath, ExternalCallableKind};
use crate::fir::{ExternalCallableId, FirCallableReferenceBinding, FirCallableReferenceTarget};
use crate::ir::{FrDispatch, FuncRef, IrCheckedOperation, IrClass, IrExpr, IrFile, IrFunction};
use crate::libraries::MemberRealization;
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
    let adaptation_flags = reference.adaptation.as_deref().map_or(0, |adaptation| {
        adapted_flags(adaptation, reference.declaration_result)
    });
    let (owner_class, name, top_level) = match reference.target {
        crate::ir::IrCallableReferenceTarget::Module(target) => {
            let declaration = ir
                .referenced_module_callables
                .get(&target)
                .ok_or(FunctionReferenceRealizationTarget::Module(target))?;
            (
                declaration.owner,
                declaration.name.to_string(),
                declaration.owner.is_none(),
            )
        }
        crate::ir::IrCallableReferenceTarget::Constructor { classifier } => {
            (Some(classifier), "<init>".to_string(), false)
        }
        crate::ir::IrCallableReferenceTarget::Local { owner, name } => {
            (owner, name.into(), owner.is_none())
        }
        // Only the native backend builds one of these: on the JVM a reflective dependency
        // reference keeps its checked node, so that this realization never has to invent an owner
        // spelling for a declaration the provider owns.
        crate::ir::IrCallableReferenceTarget::External { declaration } => {
            return Err(FunctionReferenceRealizationTarget::External(declaration))
        }
    };
    let adapted = reference.adaptation.is_some();
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

/// Whether a structural reference's adapter can become the carrier's own `invoke`. The remaining
/// shapes keep the synthesized dispatching `invoke`: suspend references (their `invoke` is a
/// coroutine entry point), `FunctionN` arities past the numbered interfaces, field captures of a
/// local function, and value-class signatures (their bridge boxes through `box-impl`).
fn own_invoke_realizable(
    ir: &IrFile,
    classifiers: &dyn crate::types::ClassifierFactSource,
    reference: &crate::ir::IrCallableReference,
) -> bool {
    let Ty::Fun(function_type) = reference.function_type.non_null() else {
        return false;
    };
    // A value class declared in another file of the module, or in a dependency, is not in this
    // file's IR; the checked classifier facts answer for it.
    let value_class = |ty: &Ty| {
        let ty = ty.non_null();
        ty.is_unsigned()
            || ty.obj_internal().is_some_and(|internal| {
                ir.is_value_class_name(internal)
                    || classifiers.classifier_value_underlying(internal).is_some()
            })
    };
    reference.captures.is_empty()
        && !function_type.suspend
        && !reference.declaration_suspend
        && !ir.suspend_funs.contains(&reference.adapter)
        && function_type.params.len() <= crate::jvm::names::MAX_NUMBERED_FUNCTION_ARITY
        && !function_type.params.iter().any(value_class)
        && !value_class(&function_type.ret)
        && ir
            .functions
            .get(reference.adapter as usize)
            .is_some_and(|adapter| {
                adapter.body.is_some()
                    && !adapter.params.iter().any(value_class)
                    && !value_class(&adapter.ret)
            })
}

/// Expressions in one function body's value namespace. A lambda's captures read this namespace,
/// while its `inline_body` uses the lambda function's own slots and return boundary; never carry a
/// callable-reference adapter rewrite through that boundary.
fn adapter_body_expressions(ir: &IrFile, body: crate::ir::ExprId) -> Vec<crate::ir::ExprId> {
    let mut seen = std::collections::HashSet::new();
    let mut pending = vec![body];
    let mut expressions = Vec::new();
    while let Some(current) = pending.pop() {
        if !seen.insert(current) {
            continue;
        }
        match &ir.exprs[current as usize] {
            IrExpr::Lambda { captures, .. } => pending.extend(captures.iter().copied()),
            _ => crate::ir::for_each_child(&ir.exprs, current, &mut |child| pending.push(child)),
        }
        expressions.push(current);
    }
    expressions
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
    let expressions = adapter_body_expressions(ir, body);
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
    // operands begin: at each parameter read, or at a constructed object's `new`.
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
            }
        }
    }
    let parameters = function_type.params.clone();
    let result = match function_type.ret {
        ret if ret.is_jvm_scalar() => Ty::nullable(ret),
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
    let param_checks = parameters
        .iter()
        .map(|ty| {
            (ty.is_reference() && !ty.upper_bound_admits_null())
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

/// Materialize the physical target for an exact provider-selected member intrinsic that has no JVM
/// method to reference. The generated static helper is an implementation detail of this backend;
/// the surrounding `FuncRef` continues to describe the original Kotlin declaration for reflection
/// and equality.
fn intrinsic_member_adapter(
    ir: &mut IrFile,
    realization: MemberRealization,
    receiver: Ty,
    parameters: &[Ty],
    result: Ty,
) -> Option<(crate::ir::FunId, String)> {
    let receiver_value = ir.add_expr(IrExpr::GetValue(0));
    let arguments = parameters
        .iter()
        .enumerate()
        .map(|(parameter, _)| ir.add_expr(IrExpr::GetValue(parameter as u32 + 1)))
        .collect::<Vec<_>>();
    let value = match realization {
        MemberRealization::Intrinsic(crate::libraries::CompilerIntrinsic::StringPlus)
            if arguments.len() == 1 =>
        {
            ir.add_expr(IrExpr::Call {
                callee: crate::ir::Callee::Intrinsic {
                    operation: crate::ir::IrIntrinsic::StringPlus,
                    ret: result,
                },
                dispatch_receiver: Some(receiver_value),
                args: arguments,
            })
        }
        MemberRealization::Intrinsic(intrinsic) => {
            let operation = super::builtin_member_operations::operation(
                ir,
                intrinsic,
                super::builtin_member_operations::BuiltinMemberOperands {
                    receiver: receiver_value,
                    receiver_ty: receiver,
                    arguments: &arguments,
                    parameters,
                    result,
                },
            )?;
            ir.add_expr(operation)
        }
        _ => return None,
    };
    let returned = ir.add_expr(IrExpr::Return(Some(value)));
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![returned],
        value: None,
    });
    let name = format!("$fir$intrinsic$fnref${}", ir.functions.len());
    let function = ir.add_fun(IrFunction {
        name: name.clone(),
        params: std::iter::once(receiver)
            .chain(parameters.iter().copied())
            .collect(),
        ret: result,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    ir.private_methods.insert(function);
    Some((function, name))
}

pub(super) fn realize(
    ir: &mut IrFile,
    classpath: &Classpath,
    classifiers: &dyn crate::types::ClassifierFactSource,
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
        if let IrExpr::CallableReference(reference) = ir.exprs[raw].clone() {
            let adapter_owner = adapter_owners.get(&reference.adapter).copied();
            let sole = adapter_uses.get(&reference.adapter) == Some(&1);
            let own_invoke = sole && own_invoke_realizable(ir, classifiers, &reference);
            realize_adapter_reference(
                ir,
                current_facade,
                raw,
                adapter_owner,
                own_invoke,
                reference,
            )?;
            continue;
        }
        let IrExpr::Checked(IrCheckedOperation::CallableReference {
            target,
            binding,
            dispatch_receiver,
            extension_receiver,
            function_type,
            substitutions: _,
            adaptation,
        }) = ir.exprs[raw].clone()
        else {
            continue;
        };
        if adaptation.is_some() || dispatch_receiver.is_some() && extension_receiver.is_some() {
            return Err(FunctionReferenceRealizationTarget::Invalid);
        }
        let FirCallableReferenceTarget::External {
            declaration,
            receiver,
            extension_receiver: target_is_extension,
            parameters,
            result,
            ..
        } = target
        else {
            return Err(FunctionReferenceRealizationTarget::Invalid);
        };
        let realization = classpath
            .external_callable(declaration)
            .ok_or(FunctionReferenceRealizationTarget::External(declaration))?;
        let callable = realization.callable;
        let Ty::Fun(reference) = function_type.non_null() else {
            return Err(FunctionReferenceRealizationTarget::Invalid);
        };
        if reference.ret != result.get() || callable.suspend != reference.suspend {
            return Err(FunctionReferenceRealizationTarget::External(declaration));
        }

        let capture = dispatch_receiver.or(extension_receiver);
        let receiver_ty = receiver.map(crate::fir::ResolvedTy::get);
        let semantic_parameters = parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect::<Vec<_>>();
        let mut local_target = None;
        let mut call_owner = Some(callable.owner);
        let mut call_name = callable.name.clone();
        let mut call_interface = callable.owner_is_interface;
        let (bound, dispatch, owner_class, flags, mut target_parameters, reference_receiver) =
            match (realization.kind, target_is_extension, binding) {
                (ExternalCallableKind::TopLevel, false, FirCallableReferenceBinding::Static) => {
                    if receiver_ty.is_some() || capture.is_some() {
                        return Err(FunctionReferenceRealizationTarget::Invalid);
                    }
                    (
                        false,
                        FrDispatch::Static,
                        Some(callable.owner),
                        1,
                        callable.physical_params.clone(),
                        false,
                    )
                }
                (ExternalCallableKind::Member, false, FirCallableReferenceBinding::Bound) => {
                    if dispatch_receiver.is_none() || extension_receiver.is_some() {
                        return Err(FunctionReferenceRealizationTarget::Invalid);
                    }
                    if callable.member_realization == MemberRealization::Dispatch {
                        (
                            true,
                            FrDispatch::VirtualBound,
                            receiver_ty.and_then(Ty::kotlin_class_internal),
                            0,
                            callable.physical_params.clone(),
                            false,
                        )
                    } else {
                        let receiver =
                            receiver_ty.ok_or(FunctionReferenceRealizationTarget::Invalid)?;
                        let (target, name) = intrinsic_member_adapter(
                            ir,
                            callable.member_realization,
                            receiver,
                            &semantic_parameters,
                            result.get(),
                        )
                        .ok_or(FunctionReferenceRealizationTarget::Invalid)?;
                        local_target = Some(target);
                        call_owner = None;
                        call_name = name;
                        call_interface = false;
                        (
                            true,
                            FrDispatch::StaticBound,
                            receiver.kotlin_class_internal(),
                            0,
                            std::iter::once(receiver)
                                .chain(semantic_parameters.iter().copied())
                                .collect(),
                            false,
                        )
                    }
                }
                (ExternalCallableKind::Member, false, FirCallableReferenceBinding::Unbound) => {
                    let receiver_ty =
                        receiver_ty.ok_or(FunctionReferenceRealizationTarget::Invalid)?;
                    if capture.is_some() || reference.params.first().copied() != Some(receiver_ty) {
                        return Err(FunctionReferenceRealizationTarget::Invalid);
                    }
                    if callable.member_realization == MemberRealization::Dispatch {
                        let mut target = Vec::with_capacity(callable.physical_params.len() + 1);
                        target.push(receiver_ty);
                        target.extend(callable.physical_params.iter().copied());
                        (
                            false,
                            FrDispatch::VirtualUnbound,
                            receiver_ty.kotlin_class_internal(),
                            0,
                            target,
                            true,
                        )
                    } else {
                        let (target, name) = intrinsic_member_adapter(
                            ir,
                            callable.member_realization,
                            receiver_ty,
                            &semantic_parameters,
                            result.get(),
                        )
                        .ok_or(FunctionReferenceRealizationTarget::Invalid)?;
                        local_target = Some(target);
                        call_owner = None;
                        call_name = name;
                        call_interface = false;
                        (
                            false,
                            FrDispatch::Static,
                            receiver_ty.kotlin_class_internal(),
                            0,
                            std::iter::once(receiver_ty)
                                .chain(semantic_parameters.iter().copied())
                                .collect(),
                            true,
                        )
                    }
                }
                (ExternalCallableKind::Extension, true, FirCallableReferenceBinding::Bound) => {
                    if extension_receiver.is_none() || dispatch_receiver.is_some() {
                        return Err(FunctionReferenceRealizationTarget::Invalid);
                    }
                    (
                        true,
                        FrDispatch::StaticBound,
                        receiver_ty.and_then(Ty::kotlin_class_internal),
                        1,
                        callable.physical_params.clone(),
                        false,
                    )
                }
                (ExternalCallableKind::Extension, true, FirCallableReferenceBinding::Unbound) => {
                    let receiver_ty =
                        receiver_ty.ok_or(FunctionReferenceRealizationTarget::Invalid)?;
                    if capture.is_some() || reference.params.first().copied() != Some(receiver_ty) {
                        return Err(FunctionReferenceRealizationTarget::Invalid);
                    }
                    (
                        false,
                        FrDispatch::Static,
                        receiver_ty.kotlin_class_internal(),
                        1,
                        callable.physical_params.clone(),
                        true,
                    )
                }
                _ => return Err(FunctionReferenceRealizationTarget::Invalid),
            };

        if parameters.len() + usize::from(reference_receiver) != reference.params.len()
            || matches!(dispatch, FrDispatch::StaticBound) && target_parameters.is_empty()
        {
            return Err(FunctionReferenceRealizationTarget::External(declaration));
        }
        let mut invoke_parameters = reference.params.clone();
        let mut invoke_result = reference.ret;
        let mut target_result = callable.physical_ret;
        if reference.suspend {
            let continuation = Ty::obj("kotlin/coroutines/Continuation");
            invoke_parameters.push(continuation);
            target_parameters.push(continuation);
            invoke_result = Ty::obj("kotlin/Any");
            target_result = Ty::obj("kotlin/Any");
        }
        let arity = u8::try_from(reference.params.len())
            .map_err(|_| FunctionReferenceRealizationTarget::External(declaration))?;
        let internal = reference_class_name(ir, current_facade, raw, "function");
        let mut class = IrClass::synthetic(internal);
        class.enclosure = reference_enclosure(ir, raw);
        class.superclass = type_name("kotlin/jvm/internal/FunctionReferenceImpl");
        class.func_ref = Some(FuncRef {
            adapted: false,
            bound,
            field_capture_count: 0,
            arity,
            is_suspend: reference.suspend,
            module_target: None,
            local_target,
            owner_class,
            fn_name: callable
                .reflection_name
                .clone()
                .unwrap_or_else(|| callable.name.clone()),
            flags,
            dispatch,
            call_owner,
            call_name,
            reflection_name: None,
            reflection_receiver_parameter: false,
            // The selected provider realization above supplies physical call parameters, while
            // callable-reference reflection identifies the Kotlin declaration. Preserve its
            // semantic signature separately so the value-class pass mangles the reflected JVM
            // signature from `Marker`, not from its already-erased `String` carrier.
            reflection_target_ret_ty: Some(result.get()),
            reflection_target_param_tys: Some(semantic_parameters),
            call_interface,
            param_tys: invoke_parameters,
            ret_ty: invoke_result,
            target_param_tys: target_parameters,
            target_ret_ty: target_result,
            unbox_params: vec![None; arity as usize],
            unbox_param_nullable: vec![false; arity as usize],
            box_ret: None,
            staticbound_recv_unbox: None,
            invoke: None,
            function_type: function_type.non_null(),
        });
        let class = ir.add_class(class);
        let carrier = match capture {
            Some(capture) => IrExpr::New {
                internal,
                args: vec![capture],
                ctor_params: Some(vec![Ty::obj("kotlin/Any")]),
                ctor_desc: None,
                external_target: None,
                defaults: Box::new([]),
                default_prefix_count: 0,
            },
            None => IrExpr::StaticInstance {
                owner: class,
                ty: class,
                field: "INSTANCE",
            },
        };
        install_carrier(ir, raw, carrier, function_type);
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
        let adapter = ir.add_fun(IrFunction {
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
        own_invoke_realizable(&ir, &crate::libraries::EmptySymbolSource, &reference)
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
                ("suspend", false),
                ("high arity", false),
                ("value class", false),
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

        let expressions = adapter_body_expressions(&ir, body);

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
