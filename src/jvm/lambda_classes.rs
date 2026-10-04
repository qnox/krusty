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

/// Exact implementation identities retained across closure-class realization. Reification may
/// normalize these bodies, but not unrelated member methods capturing the same generic parameter.
#[derive(Default)]
pub(super) struct LambdaMethods {
    functions: std::collections::HashSet<FunId>,
}

impl LambdaMethods {
    fn collect(ir: &IrFile) -> Self {
        let mut functions = ir
            .lambda_origins
            .keys()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        functions.extend(ir.runtime_reified_lambda_implementations.iter().copied());
        let enclosing_lambdas = functions
            .iter()
            .filter_map(|function| {
                let (sequence, site) = ir.lifted_functions.get(function)?;
                Some((sequence.clone(), site.path.last()?.position))
            })
            .collect::<std::collections::HashSet<_>>();
        functions.extend(
            ir.lifted_functions
                .iter()
                .filter_map(|(&function, (sequence, site))| {
                    site.path
                        .iter()
                        .any(|step| enclosing_lambdas.contains(&(sequence.clone(), step.position)))
                        .then_some(function)
                }),
        );
        Self { functions }
    }

    /// Helpers appended by the delegate realization belong to the exact owned closure class.
    pub(super) fn include_owned_methods(&mut self, ir: &IrFile) {
        for class in &ir.classes {
            if class.lambda.is_some() {
                self.functions.extend(class.methods.iter().copied());
            }
        }
    }

    pub(super) fn functions(&self) -> impl Iterator<Item = FunId> + '_ {
        self.functions.iter().copied()
    }

    #[cfg(test)]
    pub(super) fn for_test(functions: impl IntoIterator<Item = FunId>) -> Self {
        Self {
            functions: functions.into_iter().collect(),
        }
    }
}

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
    site_in_roots(ir, fid, true, None)
}

fn site_in_roots(
    ir: &IrFile,
    fid: FunId,
    every_emitted_root: bool,
    class_name: Option<TypeName>,
) -> Option<Site> {
    let mut sites = reachable_lambdas(ir, fid, every_emitted_root).into_iter();
    let (Some(node), None) = (sites.next(), sites.next()) else {
        crate::trace_compiler!(
            "suspend",
            "lambda fid={fid}: reachable lambda nodes {:?}",
            reachable_lambdas(ir, fid, every_emitted_root)
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
    // A specialized copy shares the source lambda's naming provenance. Using that name here would
    // declare the call-site class as the declaration class. The copy takes a private identity until
    // lifted caller names exist; `rename_specialized_reified_classes` applies the `$$inlined$` name.
    let class = match class_name {
        Some(class) => class,
        None => crate::jvm::local_class_names::callable_reference_name(ir, node)?,
    };
    let function_type = ir.logical_types.get(&node).copied()?;
    let fits = usize::from(*arity) <= crate::jvm::names::MAX_NUMBERED_FUNCTION_ARITY;
    (fits && !inline_call_argument(ir, node)).then(|| Site {
        node,
        class,
        function_type,
        captures: captures.clone(),
    })
}

/// The `Lambda` nodes building `fid`'s value that some emitted root still reaches. Earlier passes
/// rebuild roots into fresh nodes and leave the old ones behind in the arena, so an unreachable
/// node builds no value. Property/static initializers matter here too: a suspend lambda stored by
/// one is a class just like a lambda stored from a function body.
fn reachable_lambdas(ir: &IrFile, fid: FunId, every_emitted_root: bool) -> Vec<ExprId> {
    let mut roots = Vec::new();
    for (function, declaration) in ir.functions.iter().enumerate() {
        roots.extend(declaration.body);
        if let Some(defaults) = ir
            .fn_params
            .get(&(function as FunId))
            .and_then(|parameters| parameters.defaults.as_ref())
        {
            roots.extend(defaults.iter().flatten().copied());
        }
    }
    if every_emitted_root {
        for class in &ir.classes {
            roots.extend(class.init_body);
            roots.extend(class.super_arg_prelude.iter().copied());
            roots.extend(class.super_args.iter().copied());
            roots.extend(
                class
                    .properties
                    .iter()
                    .filter_map(|property| property.initializer),
            );
            for constructor in &class.secondary_ctors {
                roots.extend(constructor.body.iter().copied());
                roots.extend(constructor.defaults.iter().flatten().copied());
                roots.extend(constructor.delegate_prelude.iter().copied());
                roots.extend(constructor.delegate_args.iter().copied());
            }
            for entry in &class.enum_entries {
                roots.extend(entry.argument_prelude.iter().copied());
                roots.extend(entry.args.iter().copied());
            }
        }
        roots.extend(ir.statics.iter().filter_map(|property| property.init));
    }

    let mut nodes = roots
        .into_iter()
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
    nests_lifted_functions_with(ir, body, false)
}

fn nests_lifted_functions_with(ir: &IrFile, body: ExprId, allow_nested_lambdas: bool) -> bool {
    crate::ir::value_namespace_expressions(ir, body)
        .iter()
        .any(|&expression| match &ir.exprs[expression as usize] {
            // A specialized escaping implementation remains an independently emitted class whose
            // JVM plan delegates to its already-owned implementation method. It does not have to
            // move into the surrounding lambda class with ordinary lifted declarations.
            IrExpr::Lambda { impl_fn, .. } => {
                !allow_nested_lambdas && !ir.specialized_functions.contains_key(impl_fn)
            }
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
pub(super) fn realize(
    ir: &mut IrFile,
    classifiers: &dyn crate::types::ClassifierFactSource,
    facade: &str,
    delegates: &super::local_delegate_closures::Requirements,
    current_source: crate::ir::IrModuleSource,
    adapt_factory: bool,
) -> Result<LambdaMethods, ()> {
    let methods = LambdaMethods::collect(ir);
    let mut lambdas = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Lambda {
                impl_fn, sam: None, ..
            } => Some(*impl_fn),
            _ => None,
        })
        .collect::<Vec<_>>();
    lambdas.sort_unstable();
    lambdas.dedup();
    for fid in lambdas {
        let runtime_reified = ir.runtime_reified_lambda_implementations.contains(&fid);
        let delegate_source = delegates.source(ir, fid);
        // A genuine source lambda has naming/origin provenance and obeys the same class-realization
        // rule in every emitted root. Generated callable-reference adapters deliberately have no
        // lambda origin: widening their root inventory would reclassify their already-selected ABI.
        // Runtime-reified copies keep the wide inventory too even if a preceding transform moved
        // their origin record.
        let every_emitted_root = ir.lambda_origins.contains_key(&fid) || runtime_reified;
        let values = reachable_lambdas(ir, fid, every_emitted_root);
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
            || (delegate_source.is_none()
                && (!adapt_factory
                    || (!runtime_reified
                        && !signature
                            .params
                            .iter()
                            .chain([&signature.ret])
                            .any(|&ty| factory_conflict(ir, classifiers, ty)))))
        {
            continue;
        }
        let class_name = ((runtime_reified || delegate_source.is_some())
            && ir.specialized_functions.contains_key(&fid))
        .then(|| specialized_reified_placeholder(facade, fid));
        if let Some(source) = delegate_source {
            let sites = delegate_sites(ir, fid, class_name)?;
            if sites.is_empty() {
                continue;
            }
            if source != current_source && !ir.specialized_functions.contains_key(&fid) {
                // The retained source closure is emitted by its declaration file. Consumers use
                // that exact class, not another declaration of its body or private helpers.
                for site in &sites {
                    replace_value(ir, site);
                }
                ir.inline_only_fns.insert(fid);
                continue;
            }
            let body = ir.functions[fid as usize].body.ok_or(())?;
            let captures = captures(ir, fid, body, &sites[0]).ok_or(())?;
            if nests_lifted_functions_with(ir, body, runtime_reified) {
                return Err(());
            }
            realize_class(ir, fid, body, &sites[0], signature, &captures);
            if let Some(result) = super::local_delegate_closures::invoke_result(ir, fid) {
                let signed_result = ir.functions[fid as usize].ret;
                ir.functions[fid as usize].ret = result;
                // A closure does not declare the inline callable's type parameters. Its invoke
                // signs the logical source/copy result separately from the declaration slot.
                ir.signatures.insert(
                    fid,
                    crate::ir::IrGenericSig {
                        type_params: Vec::new(),
                        params: signature.params.clone(),
                        ret: Some(signed_result),
                        supers: Vec::new(),
                    },
                );
            }
            let public_inline = delegates.public_inline(ir, fid);
            let class = ir.class_id_by_name(sites[0].class).ok_or(())?;
            ir.classes[class as usize]
                .lambda
                .as_mut()
                .ok_or(())?
                .public_inline = public_inline;
            for site in sites.iter().skip(1) {
                replace_value(ir, site);
            }
            continue;
        }
        match class_shape(ir, fid, every_emitted_root, runtime_reified, class_name) {
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
    Ok(methods)
}

/// Several inline copies may construct the same retained declaration closure. They have one
/// implementation identity and one class, but each value supplies its own checked captures.
fn delegate_sites(ir: &IrFile, fid: FunId, class_name: Option<TypeName>) -> Result<Vec<Site>, ()> {
    let class = match class_name {
        Some(class) => class,
        None => super::local_class_names::lambda_class_name(ir, fid).ok_or(())?,
    };
    reachable_lambdas(ir, fid, true)
        .into_iter()
        .filter(|&node| !inline_call_argument(ir, node))
        .map(|node| {
            let IrExpr::Lambda {
                captures,
                sam: None,
                ..
            } = &ir.exprs[node as usize]
            else {
                return Err(());
            };
            Ok(Site {
                node,
                class,
                function_type: ir.logical_types.get(&node).copied().ok_or(())?,
                captures: captures.clone(),
            })
        })
        .collect()
}

/// The site, body and captures of a lambda this step realizes as a class, or the shape that keeps
/// it from doing so.
fn class_shape(
    ir: &IrFile,
    fid: FunId,
    every_emitted_root: bool,
    allow_nested_lambdas: bool,
    class_name: Option<TypeName>,
) -> Result<(Site, ExprId, Vec<Capture>), &'static str> {
    let site = site_in_roots(ir, fid, every_emitted_root, class_name)
        .ok_or("no single named value outside an inline call")?;
    let body = ir.functions[fid as usize].body.ok_or("no body")?;
    if nests_lifted_functions_with(ir, body, allow_nested_lambdas) {
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
                    crate::jvm::capture_names::lifted_receiver(ir, fid, ordinal as usize)?
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
    replace_value(ir, site);
}

fn replace_value(ir: &mut IrFile, site: &Site) {
    let internal = site.class;
    ir.exprs[site.node as usize] = if site.captures.is_empty() {
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
        public_inline: false,
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

/// A temporary identity for a specialized reified lambda. `;` cannot occur in a JVM internal name,
/// so the placeholder cannot collide with a source class or be emitted.
fn specialized_reified_placeholder(facade: &str, function: FunId) -> TypeName {
    crate::types::type_name(&format!("{facade};specialized-reified#{function}"))
}

/// Name each specialized reified lambda class from the caller's final placement.
///
/// Realization runs before lifted names exist, so the class is born under
/// [`specialized_reified_placeholder`]. Suspend copies are named by their own pass.
pub(super) fn rename_specialized_reified_classes(
    ir: &mut IrFile,
    facade: &str,
    modes: crate::jvm::ir_emit::LambdaModes,
) -> std::collections::HashMap<TypeName, TypeName> {
    let pending = ir
        .classes
        .iter()
        .filter_map(|class| {
            let invoke = class.lambda.as_ref()?.invoke;
            let from = class.fq_name_id();
            (from == specialized_reified_placeholder(facade, invoke)).then_some((invoke, from))
        })
        .collect::<Vec<_>>();
    let mut all_names = std::collections::HashMap::new();
    for (function, from) in pending {
        let name =
            crate::jvm::ir_emit::lambda_class_names::specialized_name(ir, function, facade, modes)
                .expect("a specialized reified lambda retains complete caller provenance");
        let names = std::collections::HashMap::from([(from, crate::types::type_name(&name))]);
        ir.remap_classifier_identities(&names);
        all_names.extend(names);
    }
    all_names
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

#[cfg(test)]
mod method_domain_tests {
    use super::*;
    use crate::fir::{FirLiftingSite, FirLiftingStep, SourceFileId};
    use crate::ir::IrLiftingSequence;
    use crate::lifting_provenance::LiftingCallableKind;

    #[test]
    fn keeps_exact_nested_coordinates_after_source_provenance_is_consumed() {
        let mut ir = IrFile::default();
        let sequence = IrLiftingSequence {
            source: SourceFileId::from_raw(0),
            owner: "Owner".into(),
            container: "declaration".into(),
        };
        let lambda = FirLiftingStep {
            kind: LiftingCallableKind::Lambda,
            name: None,
            position: 3,
        };
        let local = FirLiftingStep {
            kind: LiftingCallableKind::LocalFunction,
            name: Some("local".into()),
            position: 4,
        };
        for (function, path) in [
            (7, vec![lambda.clone()]),
            (8, vec![lambda, local.clone()]),
            (9, vec![local]),
        ] {
            ir.lifted_functions.insert(
                function,
                (
                    sequence.clone(),
                    FirLiftingSite {
                        owner: sequence.owner.clone(),
                        container: sequence.container.clone(),
                        path: path.into_boxed_slice(),
                        lifted: true,
                    },
                ),
            );
        }
        ir.runtime_reified_lambda_implementations.insert(7);
        let methods = LambdaMethods::collect(&ir);
        ir.lifted_functions.clear();
        ir.runtime_reified_lambda_implementations.clear();
        assert_eq!(
            methods
                .functions()
                .collect::<std::collections::HashSet<_>>(),
            [7, 8].into_iter().collect()
        );
    }

    #[test]
    fn appends_owned_helpers_without_ordinary_object_members() {
        let mut ir = IrFile::default();
        let mut closure = crate::ir::IrClass::synthetic(crate::types::type_name("Closure"));
        closure.lambda = Some(crate::ir::IrLambdaClass {
            public_inline: false,
            invoke: 3,
            function_type: Ty::fun(vec![], Ty::Unit),
            receiver_captures: vec![],
            bridge: crate::ir::IrInvokeBridge::logical(vec![], Ty::Unit),
        });
        closure.methods = vec![3, 4];
        ir.classes.push(closure);
        let mut ordinary = crate::ir::IrClass::synthetic(crate::types::type_name("OrdinaryObject"));
        ordinary.methods = vec![5];
        ir.classes.push(ordinary);
        let mut methods = LambdaMethods::default();
        methods.include_owned_methods(&ir);
        assert_eq!(
            methods
                .functions()
                .collect::<std::collections::HashSet<_>>(),
            [3, 4].into_iter().collect()
        );
    }
}
