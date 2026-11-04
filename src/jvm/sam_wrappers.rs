//! The class kotlinc writes for a function value converted to a functional interface.
//!
//! A lambda literal converted to a fun interface is an `invokedynamic` of `LambdaMetafactory`. A
//! function value that already exists (a variable, a parameter, a call result) converted to a
//! Kotlin fun interface is instead wrapped in a class kotlinc generates once per file and
//! interface (`SingleAbstractMethodLowering`):
//! `<FileFacade>$sam$<interface FQ name, dots as underscores>$0`, `final synthetic`, implementing
//! the interface and `FunctionAdapter`. Its one field holds the function value; its single method,
//! typed as the interface declares it, calls the value's `invoke`; and `equals`/`hashCode` compare
//! the wrapped values, so two wrappers of one function are equal. A function value converted to a
//! Java interface uses that same shared class under `-Xsam-conversions=class` or inside an
//! `inline` function, and then implements only the Java interface.
//!
//! Checked lowering marks such a conversion on its SAM target and captures the value as the
//! conversion's lambda. This pass replaces each such lambda with a construction of the file's
//! wrapper for the interface. It runs before the value-class pass, which then realizes the method
//! like any member taking value classes.

use crate::fx_hash::{HashMap, HashSet};

use crate::ir::{ExprId, FunId, IrCtorArg, IrExpr, IrField, IrFile, IrSamTarget};
use crate::types::{Ty, TyKind, TypeName};

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
/// that interface. `class_java_sam` is `-Xsam-conversions=class`: a function value converted to a
/// Java interface then uses the same shared class, without `FunctionAdapter`, as it does inside an
/// `inline` function in every mode.
pub(super) fn realize(
    ir: &mut IrFile,
    facade: &str,
    class_java_sam: bool,
) -> SamWrapperRealizations {
    let mut realizations = SamWrapperRealizations::default();
    let facade = crate::types::type_name(facade);
    let owners = expression_functions(ir);
    let scopes = conversion_scopes(ir, &owners);
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
            let node = ExprId::try_from(node).expect("IR expression index exceeds ExprId");
            let inline_scope = scopes.get(&node).copied();
            let foreign_facade = foreign_inline_wrapper_facade(ir, node)
                .or_else(|| foreign_declaration_wrapper_facade(ir, &owners, node));
            if !wraps(
                target,
                class_java_sam,
                inline_scope.is_some() || foreign_facade.is_some(),
            ) {
                return None;
            }
            let [value] = captures.as_slice() else {
                panic!("a checked function-value SAM conversion captures exactly its value")
            };
            let public_inline =
                inline_scope == Some(InlineScope::Published) || foreign_facade.is_some();
            let source_line = ir.source_line(node).unwrap_or(0);
            Some((
                node,
                *value,
                target.clone(),
                *arity,
                public_inline,
                foreign_facade,
                source_line,
            ))
        })
        .collect::<Vec<_>>();
    sites.sort_unstable_by_key(|(node, ..)| *node);
    let mut wrappers: HashMap<(TypeName, bool, usize), TypeName> = HashMap::default();
    for (node, value, target, arity, public_inline, foreign_facade, source_line) in sites {
        let function_arity = wrapped_function_arity(ir, value, &target, arity);
        let wrapper = match foreign_facade {
            Some(foreign_facade) => wrapper_name(
                foreign_facade,
                &target,
                public_inline,
                function_arity,
                arity,
            ),
            None => *wrappers
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
                }),
        };
        let enclosed_by_cast = conversion_is_cast_operand(ir, node);
        let constructor_parameter = foreign_facade.map(|_| {
            Ty::obj(&crate::jvm::names::function_interface_internal_name(
                function_arity,
            ))
        });
        let construction = wrapper_construction(wrapper, value, constructor_parameter);
        // An explicit cast of the conversion is the only checkcast kotlinc emits. Otherwise the
        // construction is cast to the interface it converts to.
        let construction = if enclosed_by_cast {
            ir.exprs[node as usize] = construction;
            ir.set_logical_type(node, Ty::obj_name(target.classifier));
            node
        } else {
            let construction = ir.add_expr(construction);
            ir.set_logical_type(construction, Ty::obj_name(wrapper));
            ir.exprs[node as usize] = IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::Cast,
                arg: construction,
                type_operand: Ty::obj_name(target.classifier),
            };
            ir.set_logical_type(node, Ty::obj_name(target.classifier));
            construction
        };
        if target.nullable {
            // The wrapper constructor null-checks `function`. A nullable conversion must not call
            // it when the value is null; emission branches around this exact construction.
            realizations.nullable_constructions.insert(construction);
        }
        // kotlinc gives the construction no line of its own: the line starts at the wrapped value.
        ir.take_source_line(node);
        ir.mark_positionless(node);
        ir.mark_positionless(construction);
    }
    realizations
}

fn wrapper_construction(
    wrapper: TypeName,
    value: ExprId,
    constructor_parameter: Option<Ty>,
) -> IrExpr {
    IrExpr::New {
        internal: wrapper,
        args: vec![value],
        ctor_params: constructor_parameter.map(|parameter| vec![parameter]),
        ctor_desc: None,
        external_target: None,
        defaults: Box::new([]),
        default_prefix_count: 0,
    }
}

/// The exact declaration facade whose public inline SAM wrapper a copied conversion constructs.
/// Inline-copy provenance is ordered declaration frame first, so nested expansions keep naming the
/// conversion's owning declaration rather than the caller whose body contains the copy.
fn foreign_inline_wrapper_facade(ir: &IrFile, node: ExprId) -> Option<TypeName> {
    let declaration = ir.inline_copy_provenance(node)?.first()?.function;
    ir.foreign_template_facade(declaration)
}

/// The declaration facade whose wrapper a conversion written in another file's inline function
/// constructs: that function is compiled here only for its callers to inline, and its declaring
/// file publishes the wrapper its compiled body constructs.
fn foreign_declaration_wrapper_facade(
    ir: &IrFile,
    owners: &HashMap<ExprId, FunId>,
    node: ExprId,
) -> Option<TypeName> {
    let function = *owners.get(&node)?;
    if !ir.foreign_inline_templates.contains(&function) {
        return None;
    }
    ir.foreign_template_facade(function)
}

/// The conversion node is the operand of an explicit cast, so that cast is the checkcast.
///
/// `as Object` lowers as `CastNonNull` because `Object` is non-null. Both operations are the
/// source cast; a compiler-inserted cast to the interface would be a second `checkcast`.
fn conversion_is_cast_operand(ir: &IrFile, node: ExprId) -> bool {
    ir.exprs.iter().any(|expression| {
        matches!(
            expression,
            IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::Cast | crate::ir::IrTypeOp::CastNonNull,
                arg,
                ..
            } if *arg == node
        )
    })
}

/// Whether `target` converts an existing function value into the file's shared wrapper.
///
/// A Kotlin fun interface always does. A Java interface does under `-Xsam-conversions=class`, and
/// inside an `inline` function whatever the mode: kotlinc never writes an `invokedynamic` there, so
/// a caller in another module copies the class rather than relinking a call site. Representation
/// details such as suspend, context parameters, bridge results, or arity do not select a different
/// realization.
fn wraps(target: &IrSamTarget, class_java_sam: bool, in_inline_function: bool) -> bool {
    target.wraps_function_value && (target.kotlin_interface || class_java_sam || in_inline_function)
}

/// The `inline` function a conversion is written in, by whether its callers outside the file may
/// inline it. A non-private one publishes the wrapper as `$sam$i$`, a public class; a private one
/// keeps the file's ordinary `$sam$` wrapper.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InlineScope {
    Published,
    Private,
}

/// The inline scope of each function-value conversion written in an `inline` function. A caller
/// in this file that inlines the function copies the conversion; the copy constructs the
/// declaration's wrapper, as kotlinc's copy of the declaration's bytecode does.
fn conversion_scopes(ir: &IrFile, owners: &HashMap<ExprId, FunId>) -> HashMap<ExprId, InlineScope> {
    let lambda_classes = lambda_class_scopes(ir);
    let conversions = ir
        .exprs
        .iter()
        .enumerate()
        .filter_map(|(node, expression)| match expression {
            IrExpr::Lambda {
                impl_fn,
                sam: Some(_),
                ..
            } => {
                let source = super::lambda_classes::specialization_source(ir, *impl_fn);
                Some((
                    ExprId::try_from(node).expect("IR expression index exceeds ExprId"),
                    *impl_fn,
                    source,
                ))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let declared = conversions
        .iter()
        .filter_map(|&(node, implementation, source)| {
            // An inlined copy carries its caller's expression owner but not its ABI. Only the
            // declaration implementation establishes whether this stable source is published or
            // private; every specialization below queries that decision by `source`. A same-module
            // expansion clones the conversion with the same implementation, so the copy is told
            // apart by its inline-copy mark.
            if implementation != source || ir.is_inline_copy(node) {
                return None;
            }
            let function = *owners.get(&node)?;
            Some((source, inline_scope(ir, &lambda_classes, function)?))
        })
        .collect::<HashMap<_, _>>();
    conversions
        .into_iter()
        .filter_map(|(node, _, source)| Some((node, *declared.get(&source)?)))
        .collect()
}

/// The inline scope of each realized lambda class's `invoke`: a lambda written in an inline
/// function is a class of its own, and a conversion in its body is still that function's.
fn lambda_class_scopes(ir: &IrFile) -> HashMap<FunId, InlineScope> {
    ir.classes
        .iter()
        .filter_map(|class| class.lambda.as_ref())
        .filter(|lambda| lambda.public_inline)
        .map(|lambda| {
            let scope = if lambda.public_inline_abi {
                InlineScope::Published
            } else {
                InlineScope::Private
            };
            (lambda.invoke, scope)
        })
        .collect()
}

fn inline_scope(
    ir: &IrFile,
    lambda_classes: &HashMap<FunId, InlineScope>,
    function: FunId,
) -> Option<InlineScope> {
    // Another file's inline function, retained here only to be inlined, declares its own wrapper.
    if ir.foreign_inline_templates.contains(&function) {
        return None;
    }
    if ir.inline_fns.contains(&function) {
        return Some(if ir.method_visibility(function).is_private() {
            InlineScope::Private
        } else {
            InlineScope::Published
        });
    }
    lambda_classes.get(&function).copied()
}

/// The exact function whose body each reachable expression belongs to. This identity selects the
/// inline-wrapper realization and ABI.
///
/// A lambda's `inline_body` is outside the enclosing function's value namespace, but a function
/// value converted there is still that function's conversion.
fn expression_functions(ir: &IrFile) -> HashMap<ExprId, FunId> {
    let mut owners = HashMap::default();
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
    let mut seen = HashSet::default();
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
    if let Some(TyKind::Fun(signature)) = ir.logical_type(value).map(Ty::non_null).map(Ty::kind) {
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
    facade: TypeName,
    target: &IrSamTarget,
    arity: u8,
    public_inline: bool,
    source_line: u32,
    function_arity: usize,
) -> TypeName {
    let suspend_adapted = target.suspend && function_arity != usize::from(arity) + 1;
    let name = wrapper_name(facade, target, public_inline, function_arity, arity);
    let function_type = Ty::obj(&crate::jvm::names::function_interface_internal_name(
        function_arity,
    ));
    let mut class = crate::ir::IrClass::synthetic(name);
    class.decl_line = source_line;
    class.decl_start_line = source_line;
    class.superclass = crate::types::type_name("java/lang/Object");
    class.enclosure = Some(crate::ir::IrEnclosure::File);
    let function_adapter = target.kotlin_interface;
    class.interfaces.push_name(target.classifier);
    if function_adapter {
        class.interfaces.push_name(crate::types::type_name(
            "kotlin/jvm/internal/FunctionAdapter",
        ));
    }
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
    let boxes_primitive_result = crate::jvm::override_results::boxes_sam_result(target);
    if boxes_primitive_result {
        let erased_params = target
            .declared_parameters
            .iter()
            .copied()
            .map(super::bridges::bridge_erasure)
            .collect::<Vec<_>>();
        for result in crate::jvm::override_results::sam_results_boxed_over(target) {
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
                module_name_bridge: false,
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
        function_adapter,
    });
    name
}

/// Exact compiler-generated child identity of one file facade's wrapper for `target`.
fn wrapper_name(
    facade: TypeName,
    target: &IrSamTarget,
    public_inline: bool,
    function_arity: usize,
    arity: u8,
) -> TypeName {
    let suspend_adapted = target.suspend && function_arity != usize::from(arity) + 1;
    facade.nested_child(&format!(
        "sam${}{}{}$0",
        if public_inline { "i$" } else { "" },
        wrapper_segment(target.classifier),
        if suspend_adapted {
            format!("${function_arity}")
        } else {
            String::new()
        }
    ))
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
    let invoke_result = if crate::jvm::override_results::boxes_sam_result(target) {
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
