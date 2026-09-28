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
        .unwrap_or_else(|| {
            type_name(current_facade)
                .nested_child("fir")
                .nested_child(kind)
                .nested_child(&ir.classes.len().to_string())
        })
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
            crate::jvm::names::mapped_builtin_virtual_name_of(
                callable.owner,
                &callable.name,
                &descriptor,
            ),
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
    // A shared mutable capture travels as its JVM holder, as the adapter declares it.
    let capture_types = holder_parameters(
        ir,
        reference.adapter,
        function.params.get(..reference.captures.len()).ok_or(
            FunctionReferenceRealizationTarget::Adapter(reference.adapter),
        )?,
    );
    if adapter_owner.is_some() {
        // The carrier is a separate JVM class. A class-owned common adapter therefore crosses a
        // classfile access boundary even though both artifacts represent one Kotlin lexical scope.
        // The adapter has no source declaration or metadata entry, so expose it only physically and
        // mark it synthetic rather than leaking an access-bridge decision into common lowering.
        ir.set_method_visibility(reference.adapter, crate::types::Visibility::Public);
        ir.synthetic_methods.insert(reference.adapter);
    }
    // Converting an ordinary function to a `suspend` function type adapts it even when nothing
    // else about the call changes.
    let suspend_conversion = function_type.suspend && !reference.declaration_suspend;
    let adaptation_flags = reference.adaptation.as_deref().map_or(0, |adaptation| {
        adapted_flags(adaptation, reference.declaration_result)
    }) | (i32::from(suspend_conversion) << 1);
    let capture_fields = local_capture_fields(ir, &reference).unwrap_or_else(|| {
        (0..reference.captures.len())
            .map(|index| format!("$captured${index}"))
            .collect()
    });
    let reflected = match reference.target {
        crate::ir::IrCallableReferenceTarget::Module(_) => crate::ir::ReflectedCallable::Source,
        crate::ir::IrCallableReferenceTarget::Local { function, .. } => {
            crate::ir::ReflectedCallable::LocalFunction(function)
        }
        crate::ir::IrCallableReferenceTarget::Constructor { .. } => {
            crate::ir::ReflectedCallable::Constructor
        }
        // A conversion is a compiler builtin whose reflected name and signature are fixed.
        crate::ir::IrCallableReferenceTarget::External { .. }
        | crate::ir::IrCallableReferenceTarget::FunctionValueConversion { .. } => {
            crate::ir::ReflectedCallable::Physical
        }
    };
    // A local function is reflected by the physical signature it was lifted to, in which a
    // shared mutable capture is its JVM holder.
    let lifted = match reference.target {
        crate::ir::IrCallableReferenceTarget::Local { function, .. } => {
            let lifted = ir
                .functions
                .get(function as usize)
                .ok_or(FunctionReferenceRealizationTarget::Adapter(function))?;
            Some((holder_parameters(ir, function, &lifted.params), lifted.ret))
        }
        _ => None,
    };
    let (owner_class, name, top_level, reflection_signature) = match reference.target {
        crate::ir::IrCallableReferenceTarget::Module(target) => {
            let declaration = ir
                .referenced_module_callables
                .get(&target)
                .ok_or(FunctionReferenceRealizationTarget::Module(target))?;
            // A companion-block member is reflected on the class that declared its block, like
            // any member of it; only a package declaration is owned by a file facade.
            let owner = match declaration.placement {
                crate::ir::IrStaticPlacement::CompanionBlock { declaring_class } => {
                    Some(declaring_class)
                }
                crate::ir::IrStaticPlacement::Package => declaration.owner,
            };
            (owner, declaration.name.to_string(), owner.is_none(), None)
        }
        crate::ir::IrCallableReferenceTarget::Constructor { classifier } => {
            (Some(classifier), "<init>".to_string(), false, None)
        }
        // A local function has no declaration of its own on the JVM: kotlinc reflects it on
        // `Intrinsics.Kotlin`, not top-level, under the function it was lifted to.
        crate::ir::IrCallableReferenceTarget::Local { name, .. } => (
            Some(crate::types::wk::kotlin_intrinsics_reflection_owner()),
            name.into(),
            false,
            None,
        ),
        crate::ir::IrCallableReferenceTarget::External {
            declaration,
            receiver,
        } => external_reflection(classpath, declaration, receiver)?,
        // kotlinc reflects every function-value conversion, suspend or `Unit`, as a synthesized
        // `suspendConversion<N>` compiler builtin on `Intrinsics.Kotlin`.
        crate::ir::IrCallableReferenceTarget::FunctionValueConversion { ordinal } => (
            Some(crate::types::wk::kotlin_intrinsics_reflection_owner()),
            format!("suspendConversion{ordinal}"),
            false,
            None,
        ),
    };
    let adapted = reference.adaptation.is_some() || suspend_conversion;
    let bound = reference.bound_receiver.is_some();
    let continuation = Ty::obj("kotlin/coroutines/Continuation");
    let mut invoke_parameters = function_type.params.clone();
    let mut target_parameters = holder_parameters(ir, reference.adapter, &function.params);
    let mut invoke_result = function_type.ret;
    let mut target_result = function.ret;
    if function_type.suspend {
        invoke_parameters.push(continuation);
        target_parameters.push(continuation);
        invoke_result = Ty::obj("kotlin/Any");
        target_result = Ty::obj("kotlin/Any");
    }
    let (mut reflection_parameters, mut reflection_result) = match lifted {
        Some(lifted) => lifted,
        None => (
            reference.declaration_parameters.into_vec(),
            reference.declaration_result,
        ),
    };
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
        capture_fields,
        arity,
        is_suspend: function_type.suspend,
        declaration_suspend: reference.declaration_suspend,
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

/// `parameters` of `function`, with each shared mutable capture realized as its JVM holder.
fn holder_parameters(ir: &IrFile, function: crate::ir::FunId, parameters: &[Ty]) -> Vec<Ty> {
    parameters
        .iter()
        .enumerate()
        .map(|(parameter, &ty)| {
            let ordinal = u32::try_from(parameter).expect("a function has few parameters");
            ir.shared_capture_parameters
                .get(&(function, ordinal))
                .map_or(ty, super::shared_captures::holder_ty)
        })
        .collect()
}

/// kotlinc's fields for the values a local function's reference captures: each is named after the
/// captured value, as the lifted function's own parameter is, whether it holds the value or its
/// shared mutable cell. `None` for a captured receiver, which keeps the dispatching `invoke`.
fn local_capture_fields(
    ir: &IrFile,
    reference: &crate::ir::IrCallableReference,
) -> Option<Vec<String>> {
    if reference.captures.is_empty() {
        return Some(Vec::new());
    }
    let crate::ir::IrCallableReferenceTarget::Local { function, .. } = reference.target else {
        return None;
    };
    let identities = ir.function_parameter_identities(function)?;
    (0..reference.captures.len())
        .map(|capture| {
            let identity = identities.get(capture)?;
            matches!(
                identity.role,
                crate::ir::IrParameterRole::CapturedValue { .. }
            )
            .then(|| super::parameter_names::local_variable(identity, ""))
            .flatten()
        })
        .collect()
}

/// Whether a structural reference's adapter can become the carrier's own `invoke`.
fn own_invoke_realizable(ir: &IrFile, reference: &crate::ir::IrCallableReference) -> bool {
    local_capture_fields(ir, reference).is_some()
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
    // The adapter takes its captured values first, then the bound receiver, then the function
    // type's parameters. The carrier keeps each captured value in a field of its own and the bound
    // receiver in the runtime base class's; its `invoke` takes `this` and the parameters.
    let capture_fields = ir.classes[class as usize]
        .func_ref
        .as_ref()
        .map(|reference| reference.capture_fields.clone())
        .unwrap_or_default();
    let stored = capture_fields.len() + usize::from(bound);
    let stored_reads = expressions
        .iter()
        .filter_map(|&current| match ir.exprs[current as usize] {
            IrExpr::GetValue(value) if (value as usize) < stored => Some((current, value as usize)),
            _ => None,
        })
        .collect::<Vec<_>>();
    match stored {
        0 => crate::ir::shift_value_indices(ir, body, 0, 1),
        1 => {}
        _ => crate::ir::lower_value_indices(ir, body, stored as u32, stored as u32 - 1),
    }
    let mut fields = Vec::with_capacity(stored);
    for (capture, name) in capture_fields.into_iter().enumerate() {
        // A shared mutable capture's field holds its cell, which the adapter no longer declares.
        let ordinal = u32::try_from(capture).expect("a reference has few captures");
        let ty = match ir.shared_capture_parameters.remove(&(adapter, ordinal)) {
            Some(element) => super::shared_captures::holder_ty(&element),
            None => ir.functions[adapter as usize].params[capture],
        };
        fields.push((ir.classes[class as usize].fields.len(), None));
        ir.classes[class as usize].fields.push(crate::ir::IrField {
            name,
            ty,
            constructor_store_line: 0,
            type_param: None,
            default: None,
            flags: crate::ir::IrfFlags::default(),
        });
    }
    if bound {
        let receiver_ty = ir.functions[adapter as usize].params[stored - 1];
        fields.push((ir.classes[class as usize].fields.len(), Some(receiver_ty)));
        ir.classes[class as usize].fields.push(crate::ir::IrField {
            name: "receiver".to_string(),
            ty: Ty::nullable(Ty::obj("kotlin/Any")),
            constructor_store_line: 0,
            type_param: None,
            default: None,
            flags: crate::ir::IrfFlags::default(),
        });
    }
    let mut captured_reads = Vec::new();
    for (current, value) in stored_reads {
        let (field, coerced) = fields[value];
        if coerced.is_none() {
            captured_reads.push(current);
        }
        let this = ir.add_expr(IrExpr::GetValue(0));
        let read = IrExpr::GetField {
            receiver: this,
            class,
            index: u32::try_from(field).expect("a reference carrier has few fields"),
        };
        ir.exprs[current as usize] = match coerced {
            // The runtime base class stores the bound receiver as an object.
            Some(receiver_ty) => IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::ImplicitCoercion,
                arg: ir.add_expr(read),
                type_operand: receiver_ty,
            },
            None => read,
        };
    }
    // kotlinc attributes the whole body to the reference's line, entered where the call's own
    // operands begin: at each parameter or captured value read, or at a constructed object's
    // `new`. Every other node enters it only at its dispatch, so a bound receiver's read stays
    // ahead of the line.
    if let Some(line) = ir.expr_source_lines.get(&(expression as u32)).copied() {
        // The bridge maps its whole body to the same line.
        ir.fn_decl_lines.insert(adapter, line);
        let parameter_count = function_type.params.len() as u32;
        for &current in &expressions {
            let marks = match &ir.exprs[current as usize] {
                IrExpr::GetValue(value) => (1..=parameter_count).contains(value),
                IrExpr::GetField { .. } => captured_reads.contains(&current),
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
    ir.set_method_visibility(adapter, crate::types::Visibility::Public);
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

    #[test]
    fn synthesized_reference_class_nests_under_the_facade() {
        let ir = IrFile::default();
        let function = reference_class_name(&ir, "sample/ref6044/FileKt", 3, "function");
        assert_eq!(function, type_name("sample/ref6044/FileKt$fir$function$0"));
        assert_eq!(function.render(), "sample/ref6044/FileKt$fir$function$0");

        let nested = reference_class_name(&ir, "sample/Outer$Inner", 1, "property");
        assert_eq!(nested, type_name("sample/Outer$Inner$fir$property$0"));
        assert_eq!(nested.render(), "sample/Outer$Inner$fir$property$0");

        let mut occupied = IrFile::default();
        occupied
            .classes
            .push(IrClass::synthetic(type_name("sample/Other")));
        let next = reference_class_name(&occupied, "sample/ref6044/FileKt", 0, "function");
        assert_eq!(next, type_name("sample/ref6044/FileKt$fir$function$1"));
    }

    /// What a reference's single capture is, as the lifted local function declares it.
    #[derive(Clone, Copy)]
    enum Capture {
        None,
        /// A captured value the lifted function names after its source variable.
        Named,
        /// A capture the lifted function records no source identity for.
        Unnamed,
    }

    fn own_invoke_plan(function_type: Ty, declaration_suspend: bool, capture: Capture) -> bool {
        let mut ir = IrFile::default();
        let body = ir.add_expr(IrExpr::UnitInstance);
        let Ty::Fun(signature) = function_type.non_null() else {
            unreachable!("test function type")
        };
        let mut params = signature.params.clone();
        if !matches!(capture, Capture::None) {
            params.insert(0, Ty::String);
        }
        let adapter = ir.add_fun(crate::ir::IrFunction {
            name: "selected".to_string(),
            params,
            ret: signature.ret,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        if matches!(capture, Capture::Named) {
            let mut identities = vec![crate::ir::IrParameterIdentity::captured_value(
                Some("prefix".to_string()),
                0,
                crate::ir::IrValueCapture {
                    declaration: crate::ir::IrCapturedDeclaration::Parameter,
                    capturer: crate::ir::IrCapturingCallable::LocalFunction,
                },
            )];
            identities.extend(
                (0..signature.params.len())
                    .map(|index| crate::ir::IrParameterIdentity::source(format!("value{index}"))),
            );
            ir.fn_params
                .insert(adapter, crate::ir::FnParamInfo::identities(identities));
        }
        let captures = if matches!(capture, Capture::None) {
            Vec::new()
        } else {
            vec![ir.add_expr(IrExpr::UnitInstance)]
        };
        let reference = crate::ir::IrCallableReference {
            target: crate::ir::IrCallableReferenceTarget::Local {
                owner: None,
                name: "selected".into(),
                function: adapter,
            },
            adapter,
            captures,
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
                own_invoke_plan(Ty::fun(vec![Ty::Int], Ty::Int), false, Capture::None),
            ),
            (
                "captured",
                own_invoke_plan(Ty::fun(vec![Ty::String], Ty::String), false, Capture::Named),
            ),
            (
                "unnamed capture",
                own_invoke_plan(
                    Ty::fun(vec![Ty::String], Ty::String),
                    false,
                    Capture::Unnamed,
                ),
            ),
            (
                "suspend",
                own_invoke_plan(Ty::fun_suspend(vec![Ty::Int], Ty::Int), true, Capture::None),
            ),
            (
                "high arity",
                own_invoke_plan(Ty::fun(vec![Ty::Int; 23], Ty::Int), false, Capture::None),
            ),
            (
                "value class",
                own_invoke_plan(Ty::fun(vec![Ty::UInt], Ty::String), false, Capture::None),
            ),
        ];

        assert_eq!(
            plans,
            [
                ("ordinary", true),
                ("captured", true),
                ("unnamed capture", false),
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
