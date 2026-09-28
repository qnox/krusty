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

use std::collections::HashMap;

use crate::ir::{ExprId, FunId, IrCtorArg, IrExpr, IrField, IrFile, IrSamTarget};
use crate::types::{Ty, TypeName};

/// Wrap every function value converted to a Kotlin fun interface in the file's wrapper class for
/// that interface.
pub(super) fn realize(ir: &mut IrFile, facade: &str) {
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
            let node = ExprId::try_from(node).expect("IR expression index exceeds ExprId");
            let function = *owners.get(&node)?;
            let [value] = captures.as_slice() else {
                return None;
            };
            (wraps(target, *arity) && !ir.inline_fns.contains(&function))
                .then(|| (node, *value, target.clone(), *arity))
        })
        .collect::<Vec<_>>();
    sites.sort_unstable_by_key(|(node, ..)| *node);
    let mut wrappers: HashMap<TypeName, TypeName> = HashMap::new();
    for (node, value, target, arity) in sites {
        let wrapper = *wrappers
            .entry(target.classifier)
            .or_insert_with(|| declare_wrapper(ir, facade, &target, arity));
        let construction = ir.add_expr(IrExpr::New {
            internal: wrapper,
            args: vec![value],
            ctor_params: None,
            ctor_desc: None,
            external_target: None,
            defaults: Box::new([]),
            default_prefix_count: 0,
        });
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
}

/// Whether `target` converts an existing function value to a Kotlin fun interface, as a wrapper
/// class realizes it. A suspend method, and one whose primitive result replaces a non-primitive
/// one, keep the lambda realization.
fn wraps(target: &IrSamTarget, arity: u8) -> bool {
    target.wraps_function_value
        && target.kotlin_interface
        && !target.suspend
        && !target.overrides_non_primitive_result
        && target.context_count == 0
        && usize::from(arity) <= crate::jvm::names::MAX_NUMBERED_FUNCTION_ARITY
        && target.parameter_names.len() == target.declared_parameters.len()
}

/// The function whose body each reachable expression belongs to.
fn expression_functions(ir: &IrFile) -> HashMap<ExprId, FunId> {
    let mut owners = HashMap::new();
    for (function, declaration) in ir.functions.iter().enumerate() {
        let Some(body) = declaration.body else {
            continue;
        };
        let function = FunId::try_from(function).expect("IR function index exceeds FunId");
        for node in crate::ir::value_namespace_expressions(ir, body) {
            owners.entry(node).or_insert(function);
        }
    }
    owners
}

/// Declare the file's wrapper class for `target`'s interface: its field, its constructor
/// parameter, and its method forwarding to the wrapped value's `invoke`.
fn declare_wrapper(ir: &mut IrFile, facade: &str, target: &IrSamTarget, arity: u8) -> TypeName {
    let name = crate::types::type_name(&format!(
        "{facade}$sam${}$0",
        wrapper_segment(target.classifier)
    ));
    let function_type = Ty::obj(&crate::jvm::names::function_interface_internal_name(
        usize::from(arity),
    ));
    let mut class = crate::ir::IrClass::synthetic(name);
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
        capture: None,
    });
    let class_id = ir.add_class(class);
    let method = forwarding_method(ir, class_id, name, target);
    let class = &mut ir.classes[class_id as usize];
    class.methods.push(method);
    class.sam_wrapper = Some(crate::ir::IrSamWrapperClass {
        interface: target.classifier,
        method,
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
) -> FunId {
    let this = ir.add_expr(IrExpr::GetValue(0));
    let function = ir.add_expr(IrExpr::GetField {
        receiver: this,
        class,
        index: 0,
    });
    let arguments = (1..=target.declared_parameters.len())
        .map(|parameter| {
            ir.add_expr(IrExpr::GetValue(
                u32::try_from(parameter).expect("too many SAM parameters"),
            ))
        })
        .collect();
    let invoke = ir.add_expr(IrExpr::InvokeFunction {
        func: function,
        args: arguments,
        params: target.declared_parameters.clone(),
        ret: target.declared_result,
    });
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
        crate::ir::FnParamInfo::source_names(target.parameter_names.clone()),
    );
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
