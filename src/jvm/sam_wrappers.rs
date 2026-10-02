//! The class kotlinc writes for a function value converted to a Kotlin fun interface.
//!
//! A lambda literal converted to a fun interface is an `invokedynamic` of `LambdaMetafactory`, and
//! so is any function value converted to a Java interface. A function value that already exists
//! (a variable, a parameter, a call result) converted to a Kotlin fun interface is instead wrapped
//! in a class kotlinc generates once per file and interface (`SingleAbstractMethodLowering`):
//! `<FileFacade>$sam$<interface FQ name, dots as underscores>$0`, `final synthetic`, implementing
//! the interface and `FunctionAdapter`. Its one field holds the function value; its single method,
//! typed as the interface declares it, calls the value's `invoke`; and `equals`/`hashCode` compare
//! the wrapped values, so two wrappers of one function are equal.
//!
//! Checked lowering marks such a conversion on its SAM target and captures the value as the
//! conversion's lambda. This pass replaces each such lambda with a construction of the file's
//! wrapper for the interface. It runs before the value-class pass, which then realizes the method
//! like any member taking value classes.

use std::collections::{HashMap, HashSet};

use crate::ir::{ExprId, FunId, IrCtorArg, IrExpr, IrField, IrFile, IrSamTarget};
use crate::types::{Ty, TypeName};

/// JVM-only construction plans selected while realizing Kotlin function-value SAM wrappers.
/// Common IR retains semantic nullability; this record identifies the physical `New` nodes whose
/// constructor call must be skipped when that value is null.
#[derive(Default)]
pub(crate) struct SamWrapperRealizations {
    nullable_constructions: HashSet<ExprId>,
}

impl SamWrapperRealizations {
    pub(super) fn is_nullable_construction(&self, expression: ExprId) -> bool {
        self.nullable_constructions.contains(&expression)
    }
}

/// Wrap every function value converted to a Kotlin fun interface in the file's wrapper class for
/// that interface.
pub(super) fn realize(ir: &mut IrFile, facade: &str) -> SamWrapperRealizations {
    let mut realizations = SamWrapperRealizations::default();
    let owners = expression_functions(ir);
    let mut sites = ir
        .exprs
        .iter()
        .enumerate()
        .filter_map(|(node, expression)| {
            let IrExpr::Lambda {
                captures,
                sam: Some(target),
                arity,
                ..
            } = expression
            else {
                return None;
            };
            if !wraps(target) {
                return None;
            }
            let [value] = captures.as_slice() else {
                panic!("a checked function-value SAM conversion captures exactly its value")
            };
            let node = ExprId::try_from(node).expect("IR expression index exceeds ExprId");
            let public_inline = owners
                .get(&node)
                .is_some_and(|function| ir.inline_fns.contains(function));
            let source_line = ir.expr_source_lines.get(&node).copied().unwrap_or(0);
            Some((
                node,
                *value,
                target.clone(),
                *arity,
                public_inline,
                source_line,
            ))
        })
        .collect::<Vec<_>>();
    sites.sort_unstable_by_key(|(node, ..)| *node);
    let mut wrappers: HashMap<(TypeName, bool, usize), TypeName> = HashMap::new();
    for (node, value, target, arity, public_inline, source_line) in sites {
        let function_arity = wrapped_function_arity(ir, value, &target, arity);
        let wrapper = *wrappers
            .entry((target.classifier, public_inline, function_arity))
            .or_insert_with(|| {
                declare_wrapper(
                    ir,
                    facade,
                    &target,
                    arity,
                    public_inline,
                    source_line,
                    function_arity,
                )
            });
        let construction = ir.add_expr(IrExpr::New {
            internal: wrapper,
            args: vec![value],
            ctor_params: None,
            ctor_desc: None,
            external_target: None,
            defaults: Box::new([]),
            default_prefix_count: 0,
        });
        if target.nullable {
            // The wrapper constructor null-checks `function`. A nullable conversion must not call
            // it when the value is null; emission branches around this exact construction.
            realizations.nullable_constructions.insert(construction);
        }
        ir.logical_types.insert(construction, Ty::obj_name(wrapper));
        // kotlinc casts the construction to the interface it converts to.
        ir.exprs[node as usize] = IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::Cast,
            arg: construction,
            type_operand: Ty::obj_name(target.classifier),
        };
        ir.logical_types
            .insert(node, Ty::obj_name(target.classifier));
        // kotlinc gives the construction no line of its own: the line starts at the wrapped value.
        ir.expr_source_lines.remove(&node);
    }
    realizations
}

/// Whether `target` converts an existing function value to a Kotlin fun interface. Every checked
/// conversion of this semantic shape uses the wrapper path; representation details such as suspend,
/// context parameters, bridge results, arity, or enclosing inline declarations do not select a
/// different realization.
fn wraps(target: &IrSamTarget) -> bool {
    target.wraps_function_value && target.kotlin_interface
}

/// The exact function whose body each reachable expression belongs to. This identity selects the
/// public inline-wrapper ABI; it never decides whether the conversion is realized.
///
/// A lambda's `inline_body` is outside the enclosing function's value namespace, but a function
/// value converted there is still that function's conversion.
fn expression_functions(ir: &IrFile) -> HashMap<ExprId, FunId> {
    let mut owners = HashMap::new();
    for (function, declaration) in ir.functions.iter().enumerate() {
        let Some(body) = declaration.body else {
            continue;
        };
        let function = FunId::try_from(function).expect("IR function index exceeds FunId");
        assign_expression_function(ir, body, function, &mut owners);
    }
    owners
}

fn assign_expression_function(
    ir: &IrFile,
    body: ExprId,
    function: FunId,
    owners: &mut HashMap<ExprId, FunId>,
) {
    let mut pending = vec![body];
    let mut seen = HashSet::new();
    while let Some(body) = pending.pop() {
        if !seen.insert(body) {
            continue;
        }
        for node in crate::ir::value_namespace_expressions(ir, body) {
            owners.entry(node).or_insert(function);
            if let IrExpr::Lambda {
                inline_body: Some(inline_body),
                ..
            } = ir.expr(node)
            {
                pending.push(*inline_body);
            }
        }
    }
}

/// Declare the file's wrapper class for `target`'s interface: its field, its constructor
/// parameter, and its method forwarding to the wrapped value's `invoke`.
/// Arity of the `FunctionN` stored in the wrapper.
///
/// A suspend fun interface normally stores `Function{n+1}` because the value is already suspend.
/// Suspend conversion of a regular function stores that function's own `FunctionN` and calls it
/// without the continuation (`() -> Unit` into `suspend fun invoke()` is a `Function0`).
fn wrapped_function_arity(ir: &IrFile, value: ExprId, target: &IrSamTarget, arity: u8) -> usize {
    let suspend_carrier = usize::from(arity) + usize::from(target.suspend);
    if let Some(Ty::Fun(signature)) = ir.logical_types.get(&value).copied().map(Ty::non_null) {
        if target.suspend && !signature.suspend {
            return signature.params.len();
        }
    }
    // A property reference or fun interface is not a function type, and a constructor check can
    // publish the target's suspend shape over the operand. The conversion recorded the value's
    // own suspension: a non-suspend view is stored as `Function{arity}`.
    if target.suspend && !target.source_suspend {
        return usize::from(arity);
    }
    suspend_carrier
}

fn declare_wrapper(
    ir: &mut IrFile,
    facade: &str,
    target: &IrSamTarget,
    arity: u8,
    public_inline: bool,
    source_line: u32,
    function_arity: usize,
) -> TypeName {
    let suspend_adapted = target.suspend && function_arity != usize::from(arity) + 1;
    let name = crate::types::type_name(&format!(
        "{facade}$sam${}{}{}$0",
        if public_inline { "i$" } else { "" },
        wrapper_segment(target.classifier),
        if suspend_adapted {
            format!("${function_arity}")
        } else {
            String::new()
        }
    ));
    let function_type = Ty::obj(&crate::jvm::names::function_interface_internal_name(
        function_arity,
    ));
    let mut class = crate::ir::IrClass::synthetic(name);
    class.decl_line = source_line;
    class.decl_start_line = source_line;
    class.superclass = crate::types::type_name("java/lang/Object");
    class.enclosure = Some(crate::ir::IrEnclosure::File);
    class.interfaces.push_name(target.classifier);
    class.interfaces.push_name(crate::types::type_name(
        "kotlin/jvm/internal/FunctionAdapter",
    ));
    class
        .fields
        .push(IrField::new("function".to_string(), function_type).with_is_final(true));
    class.ctor_args.push(IrCtorArg {
        name: Some("function".to_string()),
        context_kind: crate::types::ContextParameterKind::None,
        ty: function_type,
        declared_ty: None,
        is_field: false,
        field_index: None,
        has_default: false,
        is_vararg: false,
        type_param: None,
        check: None,
        anonymous_super_forward: None,
        capture: None,
        provenance: crate::ir::IrCtorParameterProvenance::Value,
        capture_identity: None,
    });
    let class_id = ir.add_class(class);
    let method = forwarding_method(ir, class_id, name, target, function_arity, suspend_adapted);
    let class = &mut ir.classes[class_id as usize];
    class.methods.push(method);
    let boxes_primitive_result = target.overrides_non_primitive_result && !target.suspend;
    if boxes_primitive_result {
        let erased_params = target
            .declared_parameters
            .iter()
            .copied()
            .map(super::bridges::bridge_erasure)
            .collect::<Vec<_>>();
        for &result in &target.overridden_non_primitive_results {
            let erased_ret = super::bridges::bridge_erasure(result);
            if class.bridges.iter().any(|bridge| {
                bridge.name == target.method
                    && bridge.erased_params == erased_params
                    && bridge.erased_ret == erased_ret
            }) {
                continue;
            }
            class.bridges.push(crate::ir::Bridge {
                kind: crate::ir::BridgeKind::Function,
                target_function: Some(method),
                parameters: target
                    .parameter_identities
                    .iter()
                    .cloned()
                    .zip(target.declared_parameters.iter().copied())
                    .map(|(identity, semantic)| crate::ir::BridgeParameter { identity, semantic })
                    .collect(),
                name: target.method.clone(),
                erased_params: erased_params.clone(),
                erased_ret,
                concrete_params: target.declared_parameters.clone(),
                concrete_ret: Ty::nullable(target.declared_result),
                target_ret: None,
                overridden_owner: None,
                collection_barrier: None,
                barrier_plan: None,
                special: false,
                target_name: None,
                property_implementation: None,
            });
        }
    }
    class.sam_wrapper = Some(crate::ir::IrSamWrapperClass {
        interface: target.classifier,
        method,
        boxes_primitive_result,
        suspend_arity: (!suspend_adapted && target.suspend).then_some(arity),
        public_inline,
    });
    name
}

/// kotlinc's spelling of the interface in the wrapper's name: its Kotlin fully qualified name with
/// every `.` an `_`.
fn wrapper_segment(interface: TypeName) -> String {
    let mut nested = Vec::new();
    let mut classifier = interface;
    while let Some(owner) = classifier.nested_owner() {
        nested.push(
            classifier
                .nested_segment_within(owner)
                .expect("a nested classifier's segment extends its owner's"),
        );
        classifier = owner;
    }
    let package = classifier.package();
    let mut parts = package
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    parts.push(classifier.segment_ref());
    parts.extend(nested.into_iter().rev());
    parts.join("_")
}

/// The wrapper's implementation of the interface method, typed as the interface declares it:
/// `this.function.invoke(arguments)`, its result returned unless the method returns `Unit`.
fn forwarding_method(
    ir: &mut IrFile,
    class: crate::ir::ClassId,
    name: TypeName,
    target: &IrSamTarget,
    function_arity: usize,
    suspend_adapted: bool,
) -> FunId {
    let this = ir.add_expr(IrExpr::GetValue(0));
    let function = ir.add_expr(IrExpr::GetField {
        receiver: this,
        class,
        index: 0,
    });
    // Suspend conversion calls the original function and ignores the continuation. The interface
    // method still receives that continuation; it is not an argument of `FunctionN.invoke`.
    let argument_count = if suspend_adapted {
        function_arity
    } else {
        target.declared_parameters.len()
    };
    let arguments = (1..=argument_count)
        .map(|parameter| {
            ir.add_expr(IrExpr::GetValue(
                u32::try_from(parameter).expect("too many SAM parameters"),
            ))
        })
        .collect();
    let invoke_result = if target.overrides_non_primitive_result && !target.suspend {
        Ty::nullable(target.declared_result)
    } else {
        target.declared_result
    };
    let mut invoke_params = target.declared_parameters.clone();
    if suspend_adapted {
        invoke_params.truncate(argument_count);
    }
    let invoke = ir.add_expr(IrExpr::InvokeFunction {
        func: function,
        args: arguments,
        params: invoke_params,
        ret: invoke_result,
    });
    if target.suspend && !suspend_adapted {
        ir.suspend_calls.insert(invoke, target.declared_result);
    }
    let statements = if target.declared_result == Ty::Unit {
        vec![invoke, ir.add_expr(IrExpr::Return(None))]
    } else {
        vec![ir.add_expr(IrExpr::Return(Some(invoke)))]
    };
    let body = ir.add_expr(IrExpr::Block {
        stmts: statements,
        value: None,
    });
    let method = ir.add_fun(crate::ir::IrFunction {
        name: target.method.clone(),
        params: target.declared_parameters.clone(),
        ret: target.declared_result,
        body: Some(body),
        is_static: false,
        dispatch_receiver: Some(name),
        param_checks: Vec::new(),
    });
    ir.fn_params.insert(
        method,
        crate::ir::FnParamInfo::identities(
            target
                .parameter_identities
                .iter()
                .map(crate::ir::IrParameterIdentity::resolved)
                .collect(),
        ),
    );
    ir.fn_source_names.insert(method, target.method.clone());
    if target.suspend {
        ir.suspend_funs.push(method);
    }
    ir.synthetic_methods.insert(method);
    ir.fn_debug_locals.insert(method);
    // kotlinc writes no nullability annotations on the wrapper's method.
    ir.jvm_nullability_unannotated_methods.insert(method);
    let mut pending = vec![body];
    while let Some(expression) = pending.pop() {
        ir.expression_owners.insert(expression, name);
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    method
}
