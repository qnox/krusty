//! kotlinc's `SuspendLambdaLowering`: a suspend lambda compiles to a class of its own, which
//! extends `SuspendLambda`, implements the lambda's `FunctionN` and is its own continuation.
//!
//! The lifted lambda function becomes the class's `invokeSuspend(Object $result)`. Its captured
//! values are the class's final `$name` fields (the captured `this` is `this$0`), which the
//! constructor stores before `SuspendLambda(arity, completion)`. The lambda's own parameters are kept
//! in spill-named fields (`L$0`, `I$0`, …, one counter per normalized kind) that `create` or
//! `invoke` fill on a fresh copy of the lambda. The body reads each back into a local at its top,
//! and the emitter marks each read with `mark(10)`. Only parameters the body reads get a field.
//! The expression that built the lambda value constructs the class with a null completion instead.
//!
//! The state machine is kotlinc's transformer's, in its lambda mode, when the class is written.
//! The lambdas taken so far are the shapes the transformer takes for a named function: every
//! suspension point is a plain call and no inline body is spliced in. The others keep today's
//! lowering, a lifted function with a continuation class of its own, until their steps land (see
//! `docs/JVM_INLINE_BEFORE_CPS.md`).

use super::bytecode_machine::{eligible_points, owned_suspensions, Route, Routed, Subject};
use super::cps::{
    SuspendLambdaCapture, SuspendLambdaClass, SuspendLambdaMachine, TransformedMachine,
};
use super::{
    append_continuation, box_returns, continuation_ty, ensure_tail_return, int_ty,
    rewrite_current_continuation,
};
use crate::ir::{
    Callee, ClassId, ExprId, IrConst, IrCtorArg, IrExpr, IrField, IrFile, IrParameterRole, IrTypeOp,
};
use crate::types::{Ty, TypeName};

const SUSPEND_LAMBDA: &str = "kotlin/coroutines/jvm/internal/SuspendLambda";

/// The expression that builds the lambda value, and what the lambda class is named and enclosed by.
struct Site {
    node: ExprId,
    class: TypeName,
    function_type: Ty,
    captures: Vec<ExprId>,
}

/// The value a captured field holds.
struct Capture {
    name: String,
    ty: Ty,
    /// Whether it is the enclosing class's `this`.
    receiver: bool,
}

/// One of the lambda's own parameters.
struct Parameter {
    ty: Ty,
    /// The local-variable name its read-back takes.
    name: Option<String>,
    /// The name Kotlin metadata records, for a value parameter: neither the receiver nor a
    /// context parameter.
    metadata_name: Option<String>,
    /// The field it is kept in, when the body reads it.
    field: Option<SpillName>,
}

/// A parameter's field: its kind's descriptor, its index within the kind, and whether it is
/// private (a lambda's extension receiver is).
struct SpillName {
    descriptor: String,
    index: usize,
    private: bool,
}

impl SpillName {
    fn field_name(&self) -> String {
        format!("{}${}", &self.descriptor[..1], self.index)
    }
}

/// Route the lifted body of a suspend lambda to kotlinc's transformer, making the lambda a class of
/// its own, when it is one of the shapes the transformer takes.
pub(super) fn route(
    ir: &mut IrFile,
    fid: u32,
    body: ExprId,
    mut route: Route<'_, '_, '_>,
) -> Routed {
    let Some(site) = site(ir, fid) else {
        crate::trace_compiler!("suspend", "suspend lambda fid={fid}: no class site");
        return Routed::NotEligible;
    };
    if nests_lifted_functions(ir, body) {
        crate::trace_compiler!(
            "suspend",
            "suspend lambda fid={fid}: nests a lifted function"
        );
        return Routed::NotEligible;
    }
    if eligible_points(ir, fid, body, &route, Subject::SuspendLambda).is_none() {
        return Routed::NotEligible;
    }
    let Some((captures, parameters)) = layout(ir, fid, body, &site) else {
        crate::trace_compiler!("suspend", "suspend lambda fid={fid}: no field layout");
        return Routed::NotEligible;
    };
    let Some(suspensions) = owned_suspensions(ir, fid, body, &mut route, Subject::SuspendLambda)
    else {
        return Routed::Failed;
    };
    let unit_return = route.context.orig_rets[fid as usize] == Ty::Unit;
    let class = declare_class(ir, &site, &captures, &parameters);
    let internal = site.class;

    // The body moves into `invokeSuspend`: `this` takes value 0 and `$result` value 1.
    let expressions = crate::ir::value_namespace_expressions(ir, body);
    crate::ir::shift_value_indices(ir, body, 0, 2);
    let first_parameter = captures.len() as u32 + 2;
    for &expression in &expressions {
        let IrExpr::GetValue(value) = ir.exprs[expression as usize] else {
            continue;
        };
        if (2..first_parameter).contains(&value) {
            let this = ir.add_expr(IrExpr::GetValue(0));
            ir.exprs[expression as usize] = IrExpr::GetField {
                receiver: this,
                class,
                index: value - 2,
            };
        }
    }
    // The lambda is its own continuation: `coroutineContext` reads `this`'s context.
    rewrite_current_continuation(ir, body, 0);
    let parameter_reads = read_parameters(ir, class, body, first_parameter, &captures, &parameters);
    for suspension in &suspensions {
        // The lambda is the continuation its suspension points pass on.
        let this = ir.add_expr(IrExpr::GetValue(0));
        let continuation = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: this,
            type_operand: continuation_ty(),
        });
        if !append_continuation(
            ir,
            suspension.call,
            continuation,
            route.outputs.default_call_operands,
        ) {
            return Routed::Failed;
        }
    }
    if !box_returns(ir, body) {
        return Routed::Failed;
    }
    // kotlinc maps a `Unit` lambda's implicit return to the body's closing `}`.
    if let (Some(unit), Some(&close)) = (
        ensure_tail_return(ir, body, unit_return),
        ir.fn_close_lines.get(&fid),
    ) {
        ir.expr_source_lines.insert(unit, close);
    }
    become_invoke_suspend(ir, fid, internal);
    enclose_in_invoke_suspend(ir, fid);
    ir.classes[class as usize].methods.push(fid);

    // The lambda value is a fresh instance with no completion. Its type is the class's: a
    // consumer that needs its `FunctionN` casts it, one that takes `Any` does not.
    let mut arguments = site.captures.clone();
    arguments.push(ir.add_expr(IrExpr::Const(IrConst::Null)));
    ir.exprs[site.node as usize] = IrExpr::New {
        internal,
        args: arguments,
        ctor_params: None,
        ctor_desc: None,
        external_target: None,
        defaults: Box::new([]),
        default_prefix_count: 0,
    };
    ir.logical_types.insert(site.node, Ty::obj_name(internal));

    let mut declared_spill_fields: Vec<(String, usize)> = Vec::new();
    for field in parameters
        .iter()
        .filter_map(|parameter| parameter.field.as_ref())
    {
        match declared_spill_fields
            .iter_mut()
            .find(|(descriptor, _)| *descriptor == field.descriptor)
        {
            Some((_, max)) => *max = (*max).max(field.index),
            None => declared_spill_fields.push((field.descriptor.clone(), field.index)),
        }
    }
    let mut next_field = 1 + captures.len() as u32;
    route.machines.record_suspend_lambda(
        internal,
        SuspendLambdaClass {
            invoke_suspend: fid,
            function_type: site.function_type,
            captures: captures
                .iter()
                .enumerate()
                .map(|(index, capture)| SuspendLambdaCapture {
                    field: index as u32,
                    ty: capture.ty,
                    receiver: capture.receiver,
                })
                .collect(),
            parameters: parameters
                .iter()
                .map(|parameter| {
                    let field = parameter.field.as_ref().map(|_| {
                        next_field += 1;
                        next_field - 1
                    });
                    (parameter.ty, field)
                })
                .collect(),
            metadata_names: parameters
                .iter()
                .filter_map(|parameter| parameter.metadata_name.clone())
                .collect(),
            type_parameters: ir.lambda_type_parameters(fid).to_vec(),
        },
    );
    route.machines.record_transformed(
        fid,
        TransformedMachine {
            continuation_class: internal.render(),
            suspensions,
            lambda: Some(SuspendLambdaMachine {
                parameter_reads,
                declared_spill_fields,
            }),
        },
    );
    Routed::Taken
}

/// `fids` with every lambda before the function whose body declares it, and otherwise in their
/// order. An enclosing body is reshaped when its own machine is built, which can copy the lambda
/// node that builds a nested lambda's value; realizing the nested lambda's class first leaves one
/// node to replace, and the enclosing body then builds an ordinary object.
pub(super) fn innermost_first(ir: &IrFile, fids: Vec<u32>) -> Vec<u32> {
    let mut enclosing = std::collections::HashMap::new();
    for (function, declaration) in ir.functions.iter().enumerate() {
        let Some(body) = declaration.body else {
            continue;
        };
        for expression in crate::ir::value_namespace_expressions(ir, body) {
            if let IrExpr::Lambda { impl_fn, .. } = ir.exprs[expression as usize] {
                enclosing.entry(impl_fn).or_insert(function as u32);
            }
        }
    }
    let depth = |mut fid: u32| {
        let mut depth = 0usize;
        let mut seen = std::collections::HashSet::new();
        while let Some(&outer) = enclosing.get(&fid) {
            if !seen.insert(outer) {
                break;
            }
            depth += 1;
            fid = outer;
        }
        depth
    };
    let mut ordered = fids;
    ordered.sort_by_key(|&fid| std::cmp::Reverse(depth(fid)));
    ordered
}

/// The one expression that builds `fid`'s lambda value, when the lambda compiles to a class of its
/// own: a plain Kotlin function value the source's naming walk named, not one an inline call's
/// splice consumes.
fn site(ir: &IrFile, fid: u32) -> Option<Site> {
    let mut sites = reachable_lambdas(ir, fid).into_iter();
    let (Some(node), None) = (sites.next(), sites.next()) else {
        crate::trace_compiler!(
            "suspend",
            "suspend lambda fid={fid}: reachable lambda nodes {:?}",
            reachable_lambdas(ir, fid)
        );
        return None;
    };
    let lambda = &ir.exprs[node as usize];
    let IrExpr::Lambda {
        arity,
        captures,
        sam: None,
        ..
    } = lambda
    else {
        return None;
    };
    let class = crate::jvm::local_class_names::callable_reference_name(ir, node)?;
    let function_type = ir.logical_types.get(&node).copied()?;
    let fits = usize::from(*arity) <= crate::jvm::names::MAX_NUMBERED_FUNCTION_ARITY;
    (fits && !inline_call_argument(ir, node)).then(|| Site {
        node,
        class,
        function_type,
        captures: captures.clone(),
    })
}

/// The `Lambda` nodes building `fid`'s value that some function body still reaches. Earlier
/// passes rebuild a body into fresh nodes and leave the old ones behind in the arena, so a node
/// that no body reaches builds no value.
fn reachable_lambdas(ir: &IrFile, fid: u32) -> Vec<ExprId> {
    let mut nodes = ir
        .functions
        .iter()
        .filter_map(|function| function.body)
        .flat_map(|body| crate::ir::value_namespace_expressions(ir, body))
        .filter(|&node| {
            matches!(ir.exprs[node as usize], IrExpr::Lambda { impl_fn, .. } if impl_fn == fid)
        })
        .collect::<Vec<_>>();
    nodes.sort_unstable();
    nodes.dedup();
    nodes
}

/// Whether the body declares a lambda or local function of its own. Its lifted function would have
/// to move into the lambda's class with it, which kotlinc does and this step does not yet.
fn nests_lifted_functions(ir: &IrFile, body: ExprId) -> bool {
    crate::ir::value_namespace_expressions(ir, body)
        .iter()
        .any(|&expression| match &ir.exprs[expression as usize] {
            IrExpr::Lambda { .. } => true,
            IrExpr::Call {
                callee:
                    Callee::Local(function)
                    | Callee::LocalDefault(function)
                    | Callee::LocalWithDefaults { function, .. },
                ..
            } => ir.lifted_functions.contains_key(function),
            _ => false,
        })
}

/// Whether `node` is an argument of a call an inline splice expands, which consumes the lambda's
/// body in place of its value.
fn inline_call_argument(ir: &IrFile, node: ExprId) -> bool {
    ir.exprs.iter().enumerate().any(|(call, expression)| {
        let IrExpr::Call { callee, args, .. } = expression else {
            return false;
        };
        let inline = matches!(
            callee,
            Callee::Static {
                inline: crate::libraries::InlineKind::MustInline,
                ..
            }
        ) || u32::try_from(call)
            .is_ok_and(|call| ir.module_inline_calls.contains(&call));
        inline && args.contains(&node)
    })
}

/// The class's captured values and the lambda's own parameters, when every one of them has the
/// identity its field or local needs.
fn layout(
    ir: &IrFile,
    fid: u32,
    body: ExprId,
    site: &Site,
) -> Option<(Vec<Capture>, Vec<Parameter>)> {
    let function = &ir.functions[fid as usize];
    let identities = &ir.fn_params.get(&fid)?.identities;
    let own_from = site.captures.len();
    if ir.lambda_own_params_from.get(&fid).copied() != Some(own_from as u32)
        || identities.len() != function.params.len()
    {
        return None;
    }
    let mut captures = Vec::with_capacity(own_from);
    for (parameter, identity) in identities[..own_from].iter().enumerate() {
        let (name, receiver) = match identity.role {
            IrParameterRole::CapturedValue { .. } => {
                (format!("${}", identity.source_name.as_ref()?), false)
            }
            IrParameterRole::CapturedReceiver { ordinal: 0 } => ("this$0".to_string(), true),
            _ => return None,
        };
        let ty = match ir.shared_capture_parameters.get(&(fid, parameter as u32)) {
            Some(element) => crate::jvm::shared_captures::holder_ty(element),
            None => function.params[parameter],
        };
        captures.push(Capture { name, ty, receiver });
    }
    let expressions = crate::ir::value_namespace_expressions(ir, body);
    let read = |value: usize| {
        expressions
            .iter()
            .any(|&e| matches!(ir.exprs[e as usize], IrExpr::GetValue(v) if v as usize == value))
    };
    // A capture is a field now; nothing may assign it.
    if expressions.iter().any(
        |&e| matches!(ir.exprs[e as usize], IrExpr::SetValue { var, .. } if (var as usize) < own_from),
    ) {
        return None;
    }
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut parameters = Vec::new();
    for (parameter, identity) in identities.iter().enumerate().skip(own_from) {
        let ty = function.params[parameter];
        let receiver = identity.role == IrParameterRole::ExtensionReceiver;
        let name = match (&identity.source_name, receiver) {
            (Some(name), _) => Some(name.clone()),
            (None, true) => Some(crate::jvm::parameter_names::lambda_receiver(
                ir.lambda_origins
                    .get(&fid)
                    .expect("a suspend lambda class is a source lambda's"),
            )),
            (None, false) if identity.role == IrParameterRole::DestructuredValue => {
                Some(crate::jvm::parameter_names::DESTRUCTURED.to_string())
            }
            (None, false) => None,
        };
        let field = if read(parameter) {
            name.as_ref()?;
            let descriptor = normalized_descriptor(ty);
            let index = match counts.iter_mut().find(|(kind, _)| *kind == descriptor) {
                Some((_, count)) => {
                    *count += 1;
                    *count - 1
                }
                None => {
                    counts.push((descriptor.clone(), 1));
                    0
                }
            };
            Some(SpillName {
                descriptor,
                index,
                private: receiver,
            })
        } else {
            None
        };
        let value_parameter = !matches!(
            identity.role,
            IrParameterRole::ExtensionReceiver
                | IrParameterRole::ContextValue
                | IrParameterRole::AnonymousContextParameter { .. }
                | IrParameterRole::ContextReceiver { .. }
        );
        // A value parameter no declaration names (a destructuring one) has no metadata name to
        // record yet, so the lambda keeps the IR machine.
        let metadata_name = match value_parameter {
            true => Some(crate::jvm::parameter_names::metadata(identity)?.to_owned()),
            false => None,
        };
        parameters.push(Parameter {
            ty,
            name,
            metadata_name,
            field,
        });
    }
    Some((captures, parameters))
}

/// kotlinc's `Type.normalize()` descriptor: every reference is `Object`, and the sub-int
/// primitives are `int`.
fn normalized_descriptor(ty: Ty) -> String {
    match crate::jvm::physical_type::ir_ty_to_jvm(&ty) {
        Ty::Boolean | Ty::Char | Ty::Byte | Ty::Short | Ty::Int => "I".to_string(),
        primitive @ (Ty::Long | Ty::Float | Ty::Double) => {
            crate::jvm::names::type_descriptor(primitive)
        }
        _ => "Ljava/lang/Object;".to_string(),
    }
}

/// The type a parameter's field is declared with: `Object` for every reference, the primitive
/// itself otherwise.
fn field_ty(ty: Ty) -> Ty {
    match crate::jvm::physical_type::ir_ty_to_jvm(&ty) {
        primitive @ (Ty::Boolean
        | Ty::Char
        | Ty::Byte
        | Ty::Short
        | Ty::Int
        | Ty::Long
        | Ty::Float
        | Ty::Double) => primitive,
        _ => Ty::nullable(Ty::obj("kotlin/Any")),
    }
}

/// Declare the lambda class: `label`, the parameters' fields and the captured values' fields, and
/// a constructor over the captured values and the completion.
fn declare_class(
    ir: &mut IrFile,
    site: &Site,
    captures: &[Capture],
    parameters: &[Parameter],
) -> ClassId {
    let mut class = crate::ir::IrClass::synthetic(site.class);
    class.superclass = crate::types::type_name(SUSPEND_LAMBDA);
    class.enclosure = ir.callable_reference_enclosures.get(&site.node).copied();
    let arity = match site.function_type.non_null() {
        Ty::Fun(signature) => signature.params.len() + 1,
        _ => unreachable!("a suspend lambda has a function type"),
    };
    class
        .interfaces
        .push(&crate::jvm::names::function_interface_internal_name(arity));
    for capture in captures {
        class
            .fields
            .push(IrField::new(capture.name.clone(), capture.ty).with_is_final(true));
    }
    class
        .fields
        .push(IrField::new("label".to_string(), int_ty()).with_is_private(false));
    for parameter in parameters {
        if let Some(field) = &parameter.field {
            class.fields.push(
                IrField::new(field.field_name(), field_ty(parameter.ty))
                    .with_is_private(field.private),
            );
        }
    }
    // The constructor stores the captured values, then calls `SuspendLambda(arity, completion)`.
    for (index, capture) in captures.iter().enumerate() {
        class.ctor_args.push(constructor_argument(capture.ty));
        class
            .pre_super_param_fields
            .push((index as u32, index as u32));
    }
    class
        .ctor_args
        .push(constructor_argument(continuation_ty()));
    class.constructor_prefix_count = captures.len() as u32;
    let arity = ir.add_expr(IrExpr::Const(IrConst::Int(arity as i32)));
    let completion = ir.add_expr(IrExpr::GetValue(captures.len() as u32 + 1));
    class.super_args = vec![arity, completion];
    class.super_ctor_params = vec![Ty::Int, continuation_ty()];
    ir.add_class(class)
}

fn constructor_argument(ty: Ty) -> IrCtorArg {
    IrCtorArg {
        name: None,
        context_kind: crate::types::ContextParameterKind::None,
        ty,
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
    }
}

/// Read each parameter the body uses back from its field into its local, at the top of the body,
/// and return those declarations in order.
fn read_parameters(
    ir: &mut IrFile,
    class: ClassId,
    body: ExprId,
    first_parameter: u32,
    captures: &[Capture],
    parameters: &[Parameter],
) -> Vec<ExprId> {
    let mut field = captures.len() as u32 + 1;
    let mut reads = Vec::new();
    for (ordinal, parameter) in parameters.iter().enumerate() {
        if parameter.field.is_none() {
            continue;
        }
        let this = ir.add_expr(IrExpr::GetValue(0));
        let stored = ir.add_expr(IrExpr::GetField {
            receiver: this,
            class,
            index: field,
        });
        field += 1;
        let init = if field_ty(parameter.ty) == Ty::nullable(Ty::obj("kotlin/Any")) {
            ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::Cast,
                arg: stored,
                type_operand: parameter.ty,
            })
        } else {
            stored
        };
        let read = ir.add_expr(IrExpr::Variable {
            index: first_parameter + ordinal as u32,
            ty: parameter.ty,
            init: Some(init),
            named: true,
        });
        if let Some(name) = &parameter.name {
            ir.value_names.insert(read, name.clone());
        }
        reads.push(read);
    }
    if let IrExpr::Block { stmts, .. } = &mut ir.exprs[body as usize] {
        stmts.splice(0..0, reads.iter().copied());
    }
    reads
}

/// Make the lifted lambda function the class's `invokeSuspend(Object $result): Object`.
/// The lambda is a scope of its own: what its body declares is enclosed by the `invokeSuspend` it
/// became, kotlinc's `EnclosingMethod` for a class declared in a suspend lambda.
fn enclose_in_invoke_suspend(ir: &mut IrFile, fid: u32) {
    let lambda = crate::ir::IrEnclosure::Lambda(fid);
    let invoke_suspend = crate::ir::IrEnclosure::Function(fid);
    let enclosures = ir
        .classes
        .iter_mut()
        .filter_map(|class| class.enclosure.as_mut())
        .chain(ir.callable_reference_enclosures.values_mut());
    for enclosure in enclosures.filter(|enclosure| **enclosure == lambda) {
        *enclosure = invoke_suspend;
    }
    ir.lambda_enclosures.remove(&fid);
}

fn become_invoke_suspend(ir: &mut IrFile, fid: u32, class: TypeName) {
    let object = Ty::nullable(Ty::obj("kotlin/Any"));
    ir.fn_params.insert(
        fid,
        crate::ir::FnParamInfo::identities(vec![crate::ir::IrParameterIdentity::generated(
            crate::ir::IrGeneratedParameterRole::Positional { ordinal: 0 },
            Some("$result".to_string()),
        )]),
    );
    for owner in &mut ir.classes {
        owner.methods.retain(|&method| method != fid);
    }
    let function = &mut ir.functions[fid as usize];
    function.name = "invokeSuspend".to_string();
    function.params = vec![object];
    function.ret = object;
    function.is_static = false;
    function.dispatch_receiver = Some(class);
    function.param_checks = vec![None];
    ir.set_method_visibility(fid, crate::types::Visibility::Public);
    ir.synthetic_methods.remove(&fid);
    ir.class_static_local_functions.remove(&fid);
    ir.lambda_own_params_from.remove(&fid);
    ir.lambda_origins.remove(&fid);
    ir.lifted_functions.remove(&fid);
    ir.shared_capture_parameters
        .retain(|&(function, _), _| function != fid);
    ir.fn_debug_locals.insert(fid);
    // kotlinc writes no nullability annotations on a lambda class's members.
    ir.jvm_nullability_unannotated_methods.insert(fid);
}
