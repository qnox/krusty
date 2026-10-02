//! The class kotlinc writes for a lambda `LambdaMetafactory` cannot build.
//!
//! kotlinc compiles a plain lambda to an `invokedynamic` of `LambdaMetafactory` unless the
//! factory cannot adapt the lambda's signature to `FunctionN.invoke` (`LambdaMetafactoryArguments`,
//! a `TypeAdaptationConstraint.CONFLICT`). The lambda then becomes a class of its own: `final`,
//! extending `Object`, implementing its function type. The lifted lambda function becomes the
//! class's `invoke`, specialized to the lambda's own signature, and an erased bridge implements
//! `FunctionN.invoke` over it. Captured values are the class's final `$name` fields (the captured
//! `this` is `this$0`), stored by the constructor before `Object()`. A lambda that captures nothing
//! is a singleton read from its `INSTANCE` field.
//!
//! The conflict taken so far is a value class whose declared underlying type is neither nullable
//! nor primitive, as a parameter or the result: the factory cannot box such a value, and it knows
//! nothing of the mangled name its specialized method takes. The class is realized before the
//! lambda gets a lifted name, since kotlinc numbers only the lambdas it lifts, and before the
//! value-class pass, which erases `invoke` and completes its bridge.

use crate::ir::{ClassId, ExprId, FunId, IrCtorArg, IrExpr, IrField, IrFile, IrParameterRole};
use crate::types::{Ty, TypeName};

/// The expression that builds a lambda value, and the class it is named as when the lambda
/// compiles to a class of its own.
pub(super) struct Site {
    pub(super) node: ExprId,
    pub(super) class: TypeName,
    pub(super) function_type: Ty,
    pub(super) captures: Vec<ExprId>,
}

/// The one expression that builds `fid`'s lambda value, when the lambda can compile to a class of
/// its own: a plain Kotlin function value the source's naming walk named, not one an inline call's
/// splice consumes.
pub(super) fn site(ir: &IrFile, fid: FunId) -> Option<Site> {
    let mut sites = reachable_lambdas(ir, fid).into_iter();
    let (Some(node), None) = (sites.next(), sites.next()) else {
        crate::trace_compiler!(
            "suspend",
            "lambda fid={fid}: reachable lambda nodes {:?}",
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
fn reachable_lambdas(ir: &IrFile, fid: FunId) -> Vec<ExprId> {
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
pub(super) fn nests_lifted_functions(ir: &IrFile, body: ExprId) -> bool {
    crate::ir::value_namespace_expressions(ir, body)
        .iter()
        .any(|&expression| match &ir.exprs[expression as usize] {
            // A specialized escaping implementation remains an independently emitted class whose
            // JVM plan delegates to its already-owned implementation method. It does not have to
            // move into the surrounding lambda class with ordinary lifted declarations.
            IrExpr::Lambda { impl_fn, .. } => !ir.specialized_functions.contains_key(impl_fn),
            IrExpr::Call {
                callee:
                    crate::ir::Callee::Local(function)
                    | crate::ir::Callee::LocalDefault(function)
                    | crate::ir::Callee::LocalWithDefaults { function, .. },
                ..
            } => ir.lifted_functions.contains_key(function),
            _ => false,
        })
}

/// Whether `node` is an argument of a call to an inline declaration, whose splice consumes the
/// lambda's body in place of its value.
fn inline_call_argument(ir: &IrFile, node: ExprId) -> bool {
    ir.exprs.iter().enumerate().any(|(call, expression)| {
        let IrExpr::Call { args, .. } = expression else {
            return false;
        };
        let call = ExprId::try_from(call).expect("IR expression index exceeds ExprId");
        (ir.inline_call_sites.contains(&call) || ir.module_inline_calls.contains(&call))
            && args.contains(&node)
    })
}

/// A captured value's field.
struct Capture {
    name: String,
    ty: Ty,
    /// Whether its constructor parameter uses kotlinc's `$receiver` spelling.
    receiver: bool,
}

/// Realize every lambda whose function type `LambdaMetafactory` cannot adapt as a class of its own.
/// A lambda of a shape this step does not realize is recorded instead, so emitting it as an
/// `invokedynamic`, which cannot link, is an error rather than a silent fallback. A lambda an inline
/// call's splice consumes is recorded too: the splice emits no value for it.
pub(super) fn realize(ir: &mut IrFile, classifiers: &dyn crate::types::ClassifierFactSource) {
    let lambdas = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Lambda {
                impl_fn, sam: None, ..
            } => Some(*impl_fn),
            _ => None,
        })
        .collect::<Vec<_>>();
    for fid in lambdas {
        let values = reachable_lambdas(ir, fid);
        let Some(function_type) = values
            .first()
            .and_then(|node| ir.logical_types.get(node).copied())
        else {
            continue;
        };
        let Ty::Fun(signature) = function_type.non_null() else {
            continue;
        };
        if signature.suspend
            || !signature
                .params
                .iter()
                .chain([&signature.ret])
                .any(|&ty| factory_conflict(ir, classifiers, ty))
        {
            continue;
        }
        match class_shape(ir, fid) {
            Ok((site, body, captures)) => realize_class(ir, fid, body, &site, signature, &captures),
            Err(shape) => {
                crate::trace_compiler!(
                    "value_classes",
                    "lambda fid={fid} needs a class but is left unrealized: {shape}"
                );
                ir.jvm_unrealized_lambda_classes.insert(fid);
            }
        }
    }
}

/// The site, body and captures of a lambda this step realizes as a class, or the shape that keeps
/// it from doing so.
fn class_shape(ir: &IrFile, fid: FunId) -> Result<(Site, ExprId, Vec<Capture>), &'static str> {
    let site = site(ir, fid).ok_or("no single named value outside an inline call")?;
    let body = ir.functions[fid as usize].body.ok_or("no body")?;
    if nests_lifted_functions(ir, body) {
        return Err("its body declares a lambda or local function");
    }
    let captures = captures(ir, fid, body, &site).ok_or("a capture has no field identity")?;
    Ok((site, body, captures))
}

/// Whether `ty`, a parameter or the result of a lambda's function type, is one `LambdaMetafactory`
/// cannot adapt: a value class whose declared underlying type is neither nullable nor a primitive
/// (kotlinc's `computeParameterTypeAdaptationConstraint`). A type-parameter underlying is
/// nullable exactly when its bound is.
fn factory_conflict(
    ir: &IrFile,
    classifiers: &dyn crate::types::ClassifierFactSource,
    ty: Ty,
) -> bool {
    let Some(classifier) = ty.non_null().obj_internal() else {
        return false;
    };
    if let Some(class) = ir
        .classes
        .iter()
        .find(|class| class.is_value && class.fq_name == classifier)
    {
        let field = class
            .fields
            .first()
            .expect("a value class has its underlying field before lambda-class realization");
        let nullable = match &field.type_param {
            Some(name) => {
                field.ty.is_nullable()
                    || class
                        .type_param_bounds
                        .iter()
                        .find(|(candidate, _)| candidate == name)
                        .is_none_or(|(_, bound)| bound.is_nullable())
            }
            None => field.ty.is_nullable() || is_primitive(field.ty),
        };
        return !nullable;
    }
    let recorded = ir.external_value_class_name(classifier).copied();
    let published = classifiers.classifier_value_underlying(classifier);
    if let (Some(recorded), Some(published)) = (recorded, published) {
        assert_eq!(
            recorded.canonical_semantic(),
            published.canonical_semantic(),
            "checked providers disagreed about one value-class declaration"
        );
    }
    recorded
        .or(published)
        .is_some_and(|underlying| !admits_null(underlying) && !is_primitive(underlying))
}

/// Whether a value class's declared underlying type admits `null`: a nullable type, or a type
/// parameter whose bound does (`Result<T>(val a: T)` over `T : Any?`).
fn admits_null(underlying: Ty) -> bool {
    match underlying {
        Ty::TyParam(_, bound) => admits_null(*bound),
        other => other.admits_null(),
    }
}

/// Kotlin's primitive types: a non-null built-in scalar, never an unsigned.
fn is_primitive(ty: Ty) -> bool {
    matches!(
        ty,
        Ty::Int | Ty::Byte | Ty::Short | Ty::Long | Ty::Float | Ty::Double | Ty::Boolean | Ty::Char
    )
}

/// The class's captured values, when every one of them has the identity its field needs and the
/// body never assigns one.
fn captures(ir: &IrFile, fid: FunId, body: ExprId, site: &Site) -> Option<Vec<Capture>> {
    let function = &ir.functions[fid as usize];
    let parameter_info = ir.fn_params.get(&fid)?;
    let identities = &parameter_info.identities;
    let own_from = site.captures.len();
    if ir.lambda_own_params_from.get(&fid).copied() != Some(own_from as u32)
        || identities.len() != function.params.len()
    {
        return None;
    }
    let assigned = crate::ir::value_namespace_expressions(ir, body)
        .iter()
        .any(|&e| matches!(ir.exprs[e as usize], IrExpr::SetValue { var, .. } if (var as usize) < own_from));
    if assigned {
        return None;
    }
    identities[..own_from]
        .iter()
        .enumerate()
        .map(|(parameter, identity)| {
            let (name, receiver) = match identity.role {
                IrParameterRole::CapturedValue { .. } => {
                    (format!("${}", identity.source_name.as_ref()?), false)
                }
                IrParameterRole::CapturedReceiver { ordinal } => {
                    let receiver = parameter_info.captured_receivers.get(ordinal as usize)?;
                    (
                        crate::jvm::capture_names::lifted_receiver_name(
                            &parameter_info.captured_receivers,
                            ordinal as usize,
                        ),
                        crate::jvm::capture_names::uses_receiver_constructor_parameter(receiver),
                    )
                }
                _ => return None,
            };
            let ty = match ir.shared_capture_parameters.get(&(fid, parameter as u32)) {
                Some(element) => super::shared_captures::holder_ty(element),
                None => function.params[parameter],
            };
            Some(Capture { name, ty, receiver })
        })
        .collect()
}

/// Declare the lambda's class, move the lambda function into it as `invoke`, and build the lambda
/// value as the class's instance.
fn realize_class(
    ir: &mut IrFile,
    fid: FunId,
    body: ExprId,
    site: &Site,
    signature: &crate::types::FnSig,
    captures: &[Capture],
) {
    let class = declare_class(ir, fid, site, signature, captures);
    // `invoke` takes `this` and the lambda's own parameters; each captured value is read from its
    // field.
    let expressions = crate::ir::value_namespace_expressions(ir, body);
    let captured_reads = expressions
        .iter()
        .filter_map(|&expression| match ir.exprs[expression as usize] {
            IrExpr::GetValue(value) if (value as usize) < captures.len() => {
                Some((expression, value))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    match captures.len() {
        0 => crate::ir::shift_value_indices(ir, body, 0, 1),
        1 => {}
        count => crate::ir::lower_value_indices(ir, body, count as u32, count as u32 - 1),
    }
    for (expression, field) in captured_reads {
        let this = ir.add_expr(IrExpr::GetValue(0));
        ir.exprs[expression as usize] = IrExpr::GetField {
            receiver: this,
            class,
            index: field,
        };
    }
    // The specialized `invoke` returns the lambda's own result: nothing for `Unit`, and a primitive
    // boxed, since it overrides the generic `R`. A value class stays its carrier, which the
    // bridge boxes.
    let result = match signature.ret {
        ret if is_primitive(ret) => Ty::nullable(ret),
        ret => ret,
    };
    for &expression in &expressions {
        let IrExpr::Return(Some(value)) = ir.exprs[expression as usize] else {
            continue;
        };
        if result == Ty::Unit && matches!(ir.exprs[value as usize], IrExpr::UnitInstance) {
            ir.exprs[expression as usize] = IrExpr::Return(None);
        } else if result != signature.ret {
            let boxed = ir.add_expr(IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::ImplicitCoercion,
                arg: value,
                type_operand: result,
            });
            ir.exprs[expression as usize] = IrExpr::Return(Some(boxed));
        }
    }
    become_invoke(ir, fid, class, captures.len(), result);
    // The body belongs to the new class: what it reaches of the enclosing class's private members
    // it reaches from outside.
    let mut pending = vec![body];
    while let Some(expression) = pending.pop() {
        ir.expression_owners.insert(expression, site.class);
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    if let Some(&line) = ir.expr_source_lines.get(&site.node) {
        // The bridge maps its whole body to the lambda's line.
        ir.fn_decl_lines.insert(fid, line);
    }
    let internal = site.class;
    ir.exprs[site.node as usize] = if captures.is_empty() {
        IrExpr::ExternalStaticInstance {
            owner: internal,
            ty: internal,
            field: "INSTANCE".to_string(),
        }
    } else {
        IrExpr::New {
            internal,
            args: site.captures.clone(),
            ctor_params: None,
            ctor_desc: None,
            external_target: None,
            defaults: Box::new([]),
            default_prefix_count: 0,
        }
    };
    // The value's type is the class's: a consumer that needs its `FunctionN` casts it.
    ir.logical_types.insert(site.node, Ty::obj_name(internal));
}

/// Declare the class: a field and a constructor parameter per captured value, the function type
/// it implements, and the record its emitter and the representation passes read.
fn declare_class(
    ir: &mut IrFile,
    fid: FunId,
    site: &Site,
    signature: &crate::types::FnSig,
    captures: &[Capture],
) -> ClassId {
    let mut class = crate::ir::IrClass::synthetic(site.class);
    class.superclass = crate::types::type_name("java/lang/Object");
    class.enclosure = ir.callable_reference_enclosures.get(&site.node).copied();
    class
        .interfaces
        .push(&crate::jvm::names::function_interface_internal_name(
            signature.params.len(),
        ));
    for capture in captures {
        class
            .fields
            .push(IrField::new(capture.name.clone(), capture.ty).with_is_final(true));
        class.ctor_args.push(IrCtorArg {
            name: None,
            context_kind: crate::types::ContextParameterKind::None,
            ty: capture.ty,
            declared_ty: None,
            is_field: false,
            field_index: None,
            has_default: false,
            is_vararg: false,
            type_param: None,
            check: None,
            anonymous_super_forward: None,
            capture: None,
            // These are the lambda's captured values, added by the JVM realization after source
            // constructor lowering. Their exact field order and receiver position live on
            // `IrLambdaClass`; they are not declared value parameters.
            provenance: crate::ir::IrCtorParameterProvenance::Capture,
            // A `ClassCaptureIdentity` coordinates source local/anonymous-class forwarding. This
            // backend-generated lambda class consumes its already-bound capture list directly.
            capture_identity: None,
        });
    }
    class.lambda = Some(crate::ir::IrLambdaClass {
        invoke: fid,
        function_type: site.function_type,
        receiver_captures: captures
            .iter()
            .enumerate()
            .filter_map(|(field, capture)| capture.receiver.then_some(field as u32))
            .collect(),
        bridge: crate::ir::IrInvokeBridge::logical(signature.params.clone(), signature.ret),
    });
    ir.add_class(class)
}

/// Make the lifted lambda function the class's `invoke` over the lambda's own parameters, taking
/// no lifted name and checking each non-null reference parameter.
fn become_invoke(ir: &mut IrFile, fid: FunId, class: ClassId, captured: usize, result: Ty) {
    let internal = ir.classes[class as usize].fq_name;
    if let Some(parameters) = ir.fn_params.get_mut(&fid) {
        parameters.identities.drain(..captured);
    }
    for owner in &mut ir.classes {
        owner.methods.retain(|&method| method != fid);
    }
    ir.class_method_owners.remove(&fid);
    if let Some((sequence, site)) = ir.lifted_functions.remove(&fid) {
        let entry = site.path.last().and_then(|step| {
            ir.lifting_sequences
                .get_mut(&sequence)
                .and_then(|entries| entries.get_mut(&step.position))
        });
        if let Some(entry) = entry {
            entry.lifted = false;
        }
    }
    ir.lifted_names.remove(&fid);
    let function = &mut ir.functions[fid as usize];
    function.params.drain(..captured);
    function.param_checks = function
        .params
        .iter()
        .map(|ty| {
            (ty.is_reference() && !ty.upper_bound_admits_null())
                .then_some(crate::ir::IrParameterCheck::NonNull)
        })
        .collect();
    function.name = "invoke".to_string();
    function.ret = result;
    function.is_static = false;
    function.dispatch_receiver = Some(internal);
    ir.set_method_visibility(fid, crate::types::Visibility::Public);
    ir.synthetic_methods.remove(&fid);
    ir.class_static_local_functions.remove(&fid);
    ir.lambda_own_params_from.remove(&fid);
    ir.lambda_origins.remove(&fid);
    ir.shared_capture_parameters
        .retain(|&(function, _), _| function != fid);
    ir.fn_debug_locals.insert(fid);
    // kotlinc writes no nullability annotations on a lambda class's members.
    ir.jvm_nullability_unannotated_methods.insert(fid);
    ir.classes[class as usize].methods.push(fid);
    ir.note_class_method(class, fid);
}
