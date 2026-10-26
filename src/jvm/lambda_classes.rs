//! The class kotlinc writes for a lambda `LambdaMetafactory` cannot build.
//!
//! kotlinc compiles a plain lambda to an `invokedynamic` of `LambdaMetafactory` unless the
//! factory cannot adapt the lambda's signature to `FunctionN.invoke` (`LambdaMetafactoryArguments`,
//! a `TypeAdaptationConstraint.CONFLICT`). The lambda then becomes a class of its own: `final`,
//! extending `Object`, implementing its function type. The lifted lambda function becomes the
//! class's `invoke`, specialized to the lambda's own signature, and an erased bridge implements
//! `FunctionN.invoke` over it. Captured values are the class's final fields, stored by the
//! constructor before `Object()`. The class records what each one captures and where the lambda was
//! lifted from; the emitter spells the fields after every realization pass, since a value-class
//! member's lambda captures its static's carrier (`$arg0`) rather than the instance (`this$0`). A lambda that captures nothing
//! is a singleton read from its `INSTANCE` field.
//!
//! High-arity lambdas also take this path because `FunctionN.invoke(Object[])` needs the packed
//! bridge the realized class writes. The adaptation conflicts taken so far: a value class whose
//! declared underlying type is neither nullable
//! nor primitive, as a parameter or the result (the factory cannot box such a value, and it knows
//! nothing of the mangled name its specialized method takes); and, before language level 2.4, a
//! `Nothing`/`Nothing?` parameter or inferred result, which maps to `Void` and has no adaptee at all.
//! A lambda written inside an `inline` function is a class whatever its signature (see
//! `inline_scope`); a caller that inlines the function constructs that declaring class.
//! A default argument of an `inline` function is a class even when the factory could adapt it:
//! the non-inline `$default` stub materializes that lambda, and kotlinc writes `FileKt$f$1`
//! rather than an `invokedynamic`. The class is realized
//! before the lambda gets a lifted name, since kotlinc numbers only the lambdas it lifts, and
//! before the value-class pass, which erases `invoke` and completes its bridge. A specialization
//! that consumed an inline-lambda capture is still constructed afresh when capture transfer leaves
//! it fieldless: the expansion created that closure value rather than reading a declaration
//! singleton.

mod inline_scope;

/// The declaration implementation at the root of a chain of call-site specializations.
pub(super) fn specialization_source(ir: &IrFile, mut function: FunId) -> FunId {
    let mut seen = crate::fx_hash::HashSet::default();
    while let Some(copy) = ir.specialized_functions.get(&function) {
        assert!(
            seen.insert(function),
            "a specialized callable has acyclic source provenance"
        );
        function = copy.source;
    }
    function
}

/// The retained inline declaration template whose expansion created `function`.
pub(super) fn specialization_template(ir: &IrFile, function: FunId) -> Option<FunId> {
    let specialization = ir.specialized_functions.get(&function)?;
    ir.checked_callable_functions
        .get(&specialization.inline_callee)
        .copied()
}

use crate::ir::{ClassId, ExprId, FunId, IrCtorArg, IrExpr, IrField, IrFile, IrParameterRole};
use crate::types::{Ty, TyKind, TypeName};

/// Exact implementation identities retained across closure-class realization. Reification may
/// normalize these bodies, but not unrelated member methods capturing the same generic parameter.
#[derive(Default)]
pub(super) struct LambdaMethods {
    functions: crate::fx_hash::HashSet<FunId>,
}

/// The semantic roles a realized class has besides the lambda's implementation.
#[derive(Clone, Default)]
struct ClassRole {
    /// The lambda is, or is nested in, an inline function's default argument.
    default: Option<InlineDefaultClassContext>,
    /// The inline function whose body declares the lambda.
    scope: Option<inline_scope::InlineScope>,
    /// The source form of a `-Xlambdas=class` Kotlin lambda.
    kotlin_lambda: Option<crate::ir::IrLambdaOrigin>,
}

#[derive(Clone, Copy)]
struct InlineDefaultClassContext {
    /// The inline declaration whose default lambda is the root of this class tree.
    root_owner: FunId,
    /// The direct default lambda records the `$default` owner; descendants instead enclose their
    /// immediately surrounding realized lambda's `invoke` method.
    direct_owner: Option<FunId>,
    parent_lambda: Option<FunId>,
}

/// Every lambda class that belongs to an inline default's materialized class tree.
///
/// The relation comes from exact lifting identities. Source spellings and generated JVM names are
/// deliberately absent: a candidate belongs to the tree when the direct default lambda's lifting
/// path is a prefix of its path, and its nearest lambda prefix is its enclosing class.
fn inline_default_class_contexts(
    ir: &IrFile,
    lambdas: &[FunId],
) -> crate::fx_hash::HashMap<FunId, InlineDefaultClassContext> {
    let direct = lambdas
        .iter()
        .filter_map(|&lambda| inline_default_owner(ir, lambda).map(|owner| (lambda, owner)))
        .collect::<crate::fx_hash::HashMap<_, _>>();
    let paths = lambdas
        .iter()
        .filter_map(|&lambda| {
            let (sequence, site) = ir.lifted_functions.get(&lambda)?;
            Some((
                lambda,
                sequence.clone(),
                site.path
                    .iter()
                    .map(|step| step.position)
                    .collect::<Vec<_>>(),
            ))
        })
        .collect::<Vec<_>>();
    let mut contexts = direct
        .iter()
        .map(|(&lambda, &owner)| {
            (
                lambda,
                InlineDefaultClassContext {
                    root_owner: owner,
                    direct_owner: Some(owner),
                    parent_lambda: None,
                },
            )
        })
        .collect::<crate::fx_hash::HashMap<_, _>>();
    for (lambda, sequence, path) in &paths {
        let root =
            paths
                .iter()
                .filter_map(|(candidate, candidate_sequence, candidate_path)| {
                    let owner = direct.get(candidate)?;
                    (candidate_sequence == sequence && path.starts_with(candidate_path))
                        .then_some((*candidate, *owner, candidate_path.len()))
                })
                .max_by_key(|(_, _, depth)| *depth);
        let Some((root_lambda, root_owner, root_depth)) = root else {
            continue;
        };
        if *lambda == root_lambda {
            continue;
        }
        let parent_lambda = paths
            .iter()
            .filter(|(candidate, candidate_sequence, candidate_path)| {
                candidate_sequence == sequence
                    && *candidate != *lambda
                    && candidate_path.len() >= root_depth
                    && candidate_path.len() < path.len()
                    && path.starts_with(candidate_path)
            })
            .max_by_key(|(_, _, candidate_path)| candidate_path.len())
            .map(|(candidate, _, _)| *candidate);
        contexts.insert(
            *lambda,
            InlineDefaultClassContext {
                root_owner,
                direct_owner: None,
                parent_lambda,
            },
        );
    }
    contexts
}

/// Whether a lambda class written in the inline function `owner` names one of its reified
/// parameters in its function type, so the class's own type must be specialized for each call.
fn type_requires_reification(ir: &IrFile, owner: FunId, function_type: Ty) -> bool {
    let reified = ir
        .signatures
        .get(&owner)
        .into_iter()
        .flat_map(|signature| &signature.type_params)
        .filter(|parameter| parameter.reified)
        .map(|parameter| parameter.semantic_name.clone())
        .collect::<Vec<_>>();
    crate::types::ty_mentions_param(function_type, &reified)
}

impl LambdaMethods {
    fn collect(ir: &IrFile) -> Self {
        let mut functions = ir
            .lambda_origins
            .keys()
            .copied()
            .collect::<crate::fx_hash::HashSet<_>>();
        functions.extend(ir.runtime_reified_lambda_implementations.iter().copied());
        let enclosing_lambdas = functions
            .iter()
            .filter_map(|function| {
                let (sequence, site) = ir.lifted_functions.get(function)?;
                Some((sequence.clone(), site.path.last()?.position))
            })
            .collect::<crate::fx_hash::HashSet<_>>();
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
    /// The copies of the value in the inline-lambda templates that enclose it. A template is
    /// emitted where its lambda is inlined, so each copy builds the same class from its own
    /// captures, which read the template's values.
    pub(super) template_copies: Vec<ExprId>,
    pub(super) class: TypeName,
    pub(super) function_type: Ty,
    pub(super) captures: Vec<ExprId>,
}

/// The one expression that builds `fid`'s lambda value, when the lambda can compile to a class of
/// its own: a plain Kotlin function value the source's naming walk named, not one an inline call's
/// splice consumes.
pub(super) fn site(ir: &IrFile, fid: FunId) -> Option<Site> {
    site_in_roots(ir, fid, true, None, None)
}

/// `inline_declaration` is the node an inline function's body declares `fid` with, when the
/// lambda is written inside one (see [`inline_scope::InlineScopes`]).
fn site_in_roots(
    ir: &IrFile,
    fid: FunId,
    every_emitted_root: bool,
    inline_declaration: Option<ExprId>,
    class_name: Option<TypeName>,
) -> Option<Site> {
    let nodes = reachable_lambdas(ir, fid, every_emitted_root);
    // A same-file caller that inlines the declaring function copies a default lambda, or a lambda
    // its body builds, into its own body, so the declaration and that caller each build the value.
    // Both are the one class.
    let shared = inline_declaration.is_some() || inline_default_owner(ir, fid).is_some();
    let node = if nodes.len() == 1 {
        nodes[0]
    } else if let Some(node) = inline_declaration.filter(|node| nodes.contains(node)) {
        node
    } else if shared {
        nodes
            .iter()
            .copied()
            .find(|&node| {
                crate::jvm::local_class_names::callable_reference_name(ir, node).is_some()
            })
            .or_else(|| nodes.first().copied())?
    } else {
        crate::trace_compiler!(
            "suspend",
            "lambda fid={fid}: reachable lambda nodes {nodes:?}"
        );
        return None;
    };
    let lambda = &ir.exprs[node as usize];
    let IrExpr::Lambda {
        captures,
        sam: None,
        ..
    } = lambda
    else {
        return None;
    };
    // A specialized copy shares the source lambda's naming provenance. Using that name here would
    // declare the call-site class as the declaration class. The copy takes a private identity until
    // lifted caller names exist; `rename_specialized_lambda_classes` applies the `$$inlined$` name.
    let class = match class_name {
        Some(class) => class,
        None => crate::jvm::local_class_names::callable_reference_name(ir, node)?,
    };
    let function_type = ir.logical_types.get(&node).copied()?;
    let mut template_copies = inline_template_copies(ir, fid, every_emitted_root);
    if shared {
        template_copies.extend(nodes.into_iter().filter(|&other| other != node));
        template_copies.sort_unstable();
        template_copies.dedup();
    }
    (!inline_call_argument(ir, node)).then(|| Site {
        node,
        template_copies,
        class,
        function_type,
        captures: captures.clone(),
    })
}

/// The values of `fid`'s lambda when every one is an argument an inline call splices: no code
/// calls the lambda, each call places its body.
pub(super) fn spliced_values(ir: &IrFile, fid: FunId) -> Option<Vec<ExprId>> {
    let nodes = reachable_lambdas(ir, fid, true);
    (!nodes.is_empty() && nodes.iter().all(|&node| inline_call_argument(ir, node))).then_some(nodes)
}

/// The `Lambda` nodes building `fid`'s value that some emitted root still reaches. Earlier passes
/// rebuild roots into fresh nodes and leave the old ones behind in the arena, so an unreachable
/// node builds no value. Property/static initializers matter here too: a suspend lambda stored by
/// one is a class just like a lambda stored from a function body.
fn reachable_lambdas(ir: &IrFile, fid: FunId, every_emitted_root: bool) -> Vec<ExprId> {
    let mut nodes = emitted_roots(ir, every_emitted_root)
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

/// [`reachable_lambdas`] for every function at once. [`realize`] asks for each lambda of the file
/// in turn; one walk of the emitted roots answers every query until a realization changes the IR,
/// where a walk per lambda would make the step quadratic in the file's size.
#[derive(Default)]
struct ReachableLambdaIndex {
    every_emitted_root: Option<crate::fx_hash::HashMap<FunId, Vec<ExprId>>>,
    function_roots: Option<crate::fx_hash::HashMap<FunId, Vec<ExprId>>>,
}

impl ReachableLambdaIndex {
    fn reachable(&mut self, ir: &IrFile, fid: FunId, every_emitted_root: bool) -> Vec<ExprId> {
        let index = if every_emitted_root {
            &mut self.every_emitted_root
        } else {
            &mut self.function_roots
        };
        index
            .get_or_insert_with(|| reachable_lambda_index(ir, every_emitted_root))
            .get(&fid)
            .cloned()
            .unwrap_or_default()
    }

    /// Forget every answer once the IR may have changed.
    fn invalidate(&mut self) {
        *self = Self::default();
    }
}

fn reachable_lambda_index(
    ir: &IrFile,
    every_emitted_root: bool,
) -> crate::fx_hash::HashMap<FunId, Vec<ExprId>> {
    let mut index = crate::fx_hash::HashMap::<FunId, Vec<ExprId>>::default();
    for root in emitted_roots(ir, every_emitted_root) {
        for node in crate::ir::value_namespace_expressions(ir, root) {
            if let IrExpr::Lambda { impl_fn, .. } = ir.exprs[node as usize] {
                index.entry(impl_fn).or_default().push(node);
            }
        }
    }
    for nodes in index.values_mut() {
        nodes.sort_unstable();
        nodes.dedup();
    }
    index
}

/// The `Lambda` nodes building `fid`'s value inside the `inline_body` template of a lambda that
/// some emitted root reaches, at any depth. A template numbers its own values, so the namespace
/// walk of [`reachable_lambdas`] does not enter it.
fn inline_template_copies(ir: &IrFile, fid: FunId, every_emitted_root: bool) -> Vec<ExprId> {
    let mut pending = Vec::new();
    let enclosed_templates = |namespace: &[ExprId], pending: &mut Vec<ExprId>| {
        for &node in namespace {
            if let IrExpr::Lambda {
                inline_body: Some(template),
                ..
            } = ir.exprs[node as usize]
            {
                pending.push(template);
            }
        }
    };
    for root in emitted_roots(ir, every_emitted_root) {
        enclosed_templates(
            &crate::ir::value_namespace_expressions(ir, root),
            &mut pending,
        );
    }
    let mut seen = crate::fx_hash::HashSet::default();
    let mut copies = Vec::new();
    while let Some(template) = pending.pop() {
        if !seen.insert(template) {
            continue;
        }
        let namespace = crate::ir::value_namespace_expressions(ir, template);
        copies.extend(namespace.iter().copied().filter(|&node| {
            matches!(ir.exprs[node as usize], IrExpr::Lambda { impl_fn, .. } if impl_fn == fid)
        }));
        enclosed_templates(&namespace, &mut pending);
    }
    copies.sort_unstable();
    copies.dedup();
    copies
}

/// The bodies and initializers emission writes, each the root of one value namespace.
pub(super) fn emitted_roots(ir: &IrFile, every_emitted_root: bool) -> Vec<ExprId> {
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
        roots.extend(ir.class_ctor_default_roots());
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
    roots
}

/// Whether the body declares a lambda whose independently emitted implementation cannot move with
/// this class. Lifted local declarations are adopted by [`realize_class`] through their stable
/// lifting identities.
fn nests_unrealized_lambda(ir: &IrFile, body: ExprId, allow_nested_lambdas: bool) -> bool {
    crate::ir::value_namespace_expressions(ir, body)
        .iter()
        .any(|&expression| match &ir.exprs[expression as usize] {
            // A specialized escaping implementation remains an independently emitted class whose
            // JVM plan delegates to its already-owned implementation method. It does not have to
            // move into the surrounding lambda class with ordinary lifted declarations.
            IrExpr::Lambda { impl_fn, .. } => {
                !allow_nested_lambdas && !ir.specialized_functions.contains_key(impl_fn)
            }
            _ => false,
        })
}

/// The `inline` function whose default argument `fid` implements, when it is one.
///
/// The `$default` stub is not an inline call, so it has to materialize the lambda. kotlinc writes
/// that value as a class (`FileKt$f$1`), a singleton when the lambda captures nothing, and names
/// the stub as the class's `EnclosingMethod`.
pub(in crate::jvm) fn inline_default_owner(ir: &IrFile, fid: FunId) -> Option<FunId> {
    default_argument_owner(ir, fid).filter(|function| ir.inline_fns.contains(function))
}

/// The function whose default argument `fid` implements, inline or not.
fn default_argument_owner(ir: &IrFile, fid: FunId) -> Option<FunId> {
    ir.fn_params.iter().find_map(|(function, info)| {
        info.defaults
            .as_ref()
            .is_some_and(|defaults| {
                defaults
                    .iter()
                    .flatten()
                    .any(|&expression| default_expression_lambda(ir, expression) == Some(fid))
            })
            .then_some(*function)
    })
}

/// Whether `function` declares a default lambda that its JVM `$default` stub must materialize.
/// Lambda-class realization records the exact owner before it replaces the checked lambda
/// expression; later emission consumes that relation and does not search the rewritten body.
pub(in crate::jvm) fn has_inline_default_lambda(ir: &IrFile, function: FunId) -> bool {
    ir.classes.iter().any(|class| {
        class
            .lambda
            .as_ref()
            .is_some_and(|lambda| lambda.inline_default_owner == Some(function))
    })
}

/// The lambda a default expression materializes, if it is one.
fn default_expression_lambda(ir: &IrFile, expression: ExprId) -> Option<FunId> {
    match ir.exprs.get(expression as usize) {
        Some(IrExpr::Lambda { impl_fn, .. }) => Some(*impl_fn),
        _ => None,
    }
}

/// Whether `node` is an argument of a call to an inline declaration, whose splice consumes the
/// lambda's body in place of its value.
fn inline_call_argument(ir: &IrFile, node: ExprId) -> bool {
    ir.exprs.iter().enumerate().any(|(call, expression)| {
        let call = ExprId::try_from(call).expect("IR expression index exceeds ExprId");
        if let Some(operands) = ir.compiled_inline_operands(call) {
            // A `noinline` literal of a call inlined from a compiled body is a value the body
            // receives, so it keeps its class.
            return operands
                .arguments
                .iter()
                .enumerate()
                .any(|(parameter, &argument)| {
                    argument == node
                        && crate::jvm::inline_parameters::is_inline_parameter(ir, call, parameter)
                            == Some(true)
                });
        }
        let IrExpr::Call { args, .. } = expression else {
            return false;
        };
        // So is one of a classpath call whose published modifiers mark it `noinline`.
        (ir.inline_call_sites.contains(&call) || ir.module_inline_calls.contains(&call))
            && args.iter().enumerate().any(|(parameter, &argument)| {
                argument == node
                    && crate::jvm::inline_parameters::is_inline_parameter(ir, call, parameter)
                        != Some(false)
            })
    })
}

/// A captured value's field.
struct Capture {
    capture: crate::ir::IrLambdaCapture,
    ty: Ty,
    /// The exact runtime holder identity when this field stores a shared mutable cell.
    shared_holder: Option<TypeName>,
}

/// Realize every lambda whose function type needs a class, including `FunctionN`'s packed bridge
/// and shapes `LambdaMetafactory` cannot adapt.
/// A lambda of a shape this step does not realize is recorded instead, so emitting it as an
/// `invokedynamic`, which cannot link, is an error rather than a silent fallback. A lambda an inline
/// call's splice consumes is recorded too: the splice emits no value for it.
pub(super) fn realize(
    ir: &mut IrFile,
    classifiers: &dyn crate::types::ClassifierFactSource,
    facade: &str,
    delegates: &super::local_delegate_closures::Requirements,
    current_source: crate::ir::IrModuleSource,
    lambdas_mode: crate::jvm::ir_emit::LambdaMode,
) -> Result<LambdaMethods, ()> {
    // Under `indy` a lambda is a class only where the runtime factory cannot build it; under
    // `class` every Kotlin function lambda is one, extending `kotlin.jvm.internal.Lambda`.
    let class_lambdas = lambdas_mode == crate::jvm::ir_emit::LambdaMode::Class;
    let adapt_factory = !class_lambdas;
    let methods = LambdaMethods::collect(ir);
    let mut kotlin_lambda_sites = Vec::new();
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
    let inline_default_classes = inline_default_class_contexts(ir, &lambdas);
    let inline_scopes = inline_scope::InlineScopes::new(ir);
    let compiled_callees = ir
        .compiled_inline_calls()
        .map(|(_, function)| function)
        .collect::<crate::fx_hash::HashSet<_>>();
    let mut reachable = ReachableLambdaIndex::default();
    for fid in lambdas {
        let runtime_reified = ir.runtime_reified_lambda_implementations.contains(&fid);
        let inline_anonymous = ir.inline_anonymous_lambdas.contains(&fid);
        let inline_default = inline_default_classes.get(&fid).copied();
        // A lambda an inline call splices, or one an inline expansion took the body of, is no value
        // and has no class.
        let inline_scope = if ir.specialized_functions.contains_key(&fid) {
            inline_scopes.specialization_scope(ir, fid)
        } else {
            inline_scopes.inline_scope(ir, fid)
        }
        .filter(|scope| {
            ir.functions[fid as usize].body.is_some() && !inline_call_argument(ir, scope.node)
        });
        let inline_scoped = inline_scope.is_some();
        let delegate_source = delegates.source(ir, fid);
        // A genuine source lambda has naming/origin provenance and obeys the same class-realization
        // rule in every emitted root. Generated callable-reference adapters deliberately have no
        // lambda origin: widening their root inventory would reclassify their already-selected ABI.
        // Runtime-reified copies keep the wide inventory too even if a preceding transform moved
        // their origin record.
        let every_emitted_root =
            ir.lambda_origins.contains_key(&fid) || runtime_reified || inline_anonymous;
        let values = reachable.reachable(ir, fid, every_emitted_root);
        let Some(function_type) = values
            .first()
            .and_then(|node| ir.logical_types.get(node).copied())
        else {
            continue;
        };
        let TyKind::Fun(signature) = function_type.non_null().kind() else {
            continue;
        };
        let high_arity = crate::jvm::names::uses_function_n(signature.params.len());
        if signature.suspend
            || (delegate_source.is_none()
                && inline_default.is_none()
                && !inline_scoped
                && !class_lambdas
                && !high_arity
                && (!adapt_factory
                    || (!runtime_reified
                        && !inline_anonymous
                        && !signature
                            .params
                            .iter()
                            .chain([&signature.ret])
                            .any(|&ty| factory_conflict(ir, classifiers, ty))
                        && !nothing_conflict(ir, fid, signature))))
        {
            continue;
        }
        // Every step past here may realize a class or replace a value.
        reachable.invalidate();
        if let Some(scope) = inline_scope.filter(|scope| {
            scope.foreign
                && !ir.specialized_functions.contains_key(&fid)
                && !compiled_callees.contains(&scope.owner)
        }) {
            // The declaring file writes the class; here only its constructor is known, from the
            // captured values' types. A callee this file inlines from its compiled body realizes
            // the class in full instead, as kotlinc's `IrSourceCompilerForInline` generates the
            // callee's classes with it: the inliner reads them, and that file still writes them.
            let site = site_in_roots(ir, fid, every_emitted_root, Some(scope.node), None);
            let constructor = site.as_ref().and_then(|site| {
                let body = ir.functions[fid as usize].body?;
                captures(ir, fid, body, site)
            });
            match site.zip(constructor) {
                Some((site, captures)) => {
                    let parameters = captures.iter().map(|capture| capture.ty).collect();
                    construct(ir, &site, Some(parameters), false);
                    ir.inline_only_fns.insert(fid);
                    unlift(ir, fid);
                }
                None => {
                    ir.jvm_unrealized_lambda_classes.insert(fid);
                }
            }
            continue;
        }
        let class_name = ir
            .specialized_functions
            .contains_key(&fid)
            .then(|| specialized_lambda_placeholder(facade, fid));
        if let Some(source) = delegate_source {
            let sites = delegate_sites(ir, fid, class_name)?;
            if sites.is_empty() {
                continue;
            }
            if source != current_source
                && !ir.specialized_functions.contains_key(&fid)
                && !inline_scope.is_some_and(|scope| compiled_callees.contains(&scope.owner))
            {
                // The retained source closure is emitted by its declaration file. Consumers use
                // that exact class, not another declaration of its body or private helpers. A
                // callee inlined from its compiled body realizes it in full, as any of its lambdas.
                for site in &sites {
                    replace_value(ir, site, false);
                }
                ir.inline_only_fns.insert(fid);
                continue;
            }
            let body = ir.functions[fid as usize].body.ok_or(())?;
            let captures = captures(ir, fid, body, &sites[0]).ok_or(())?;
            if nests_unrealized_lambda(ir, body, runtime_reified) {
                return Err(());
            }
            realize_class(
                ir,
                fid,
                body,
                &sites[0],
                signature,
                &captures,
                // The inline function a closure is written in owns it as it owns any lambda.
                ClassRole {
                    scope: inline_scope,
                    ..ClassRole::default()
                },
            );
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
            let fresh_instance = ir
                .specialized_functions
                .get(&fid)
                .is_some_and(|specialization| specialization.consumed_inline_capture);
            for site in sites.iter().skip(1) {
                replace_value(ir, site, fresh_instance);
            }
            continue;
        }
        let source_class_lambda = class_lambdas || high_arity;
        let kotlin_lambda = if source_class_lambda {
            ir.lambda_origins.get(&fid).cloned()
        } else {
            None
        };
        match class_shape(
            ir,
            fid,
            every_emitted_root,
            inline_scope.map(|scope| scope.node),
            runtime_reified || inline_default.is_some() || inline_scoped || source_class_lambda,
            class_name,
        ) {
            Ok((site, body, captures)) => {
                if source_class_lambda {
                    kotlin_lambda_sites.push((site.class, site.node));
                }
                realize_class(
                    ir,
                    fid,
                    body,
                    &site,
                    signature,
                    &captures,
                    ClassRole {
                        default: inline_default,
                        scope: inline_scope,
                        kotlin_lambda,
                    },
                );
            }
            Err(shape) => {
                crate::trace_compiler!(
                    "value_classes",
                    "lambda fid={fid} needs a class but is left unrealized: {shape}"
                );
                ir.jvm_unrealized_lambda_classes.insert(fid);
            }
        }
    }
    enclose_in_lambda_classes(ir, &kotlin_lambda_sites);
    Ok(methods)
}

/// A `-Xlambdas=class` lambda is a declaration scope: a lambda class built in another lambda's body
/// is enclosed by that lambda's class `invoke`, where an implementation method would leave it to
/// the scope the outer lambda is written in.
fn enclose_in_lambda_classes(ir: &mut IrFile, sites: &[(TypeName, ExprId)]) {
    let invokes = ir
        .classes
        .iter()
        .filter_map(|class| class.lambda.as_ref())
        .map(|lambda| lambda.invoke)
        .collect::<Vec<_>>();
    let mut owners = crate::fx_hash::HashMap::default();
    for invoke in invokes {
        let Some(body) = ir.functions[invoke as usize].body else {
            continue;
        };
        for expression in crate::ir::value_namespace_expressions(ir, body) {
            owners.insert(expression, invoke);
        }
    }
    for &(class, node) in sites {
        let (Some(&invoke), Some(class)) = (owners.get(&node), ir.class_id_by_name(class)) else {
            continue;
        };
        ir.classes[class as usize].enclosure = Some(crate::ir::IrEnclosure::Function(invoke));
    }
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
                template_copies: Vec::new(),
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
    inline_declaration: Option<ExprId>,
    allow_nested_lambdas: bool,
    class_name: Option<TypeName>,
) -> Result<(Site, ExprId, Vec<Capture>), &'static str> {
    let site = site_in_roots(ir, fid, every_emitted_root, inline_declaration, class_name)
        .ok_or("no single named value outside an inline call")?;
    let body = ir.functions[fid as usize].body.ok_or("no body")?;
    if nests_unrealized_lambda(ir, body, allow_nested_lambdas) {
        return Err("its body declares an unrealized lambda");
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

/// Whether the lambda's implementation has a `Nothing`/`Nothing?` type the pre-2.4 factory cannot adapt
/// (kotlinc's `computeParameterTypeAdaptationConstraint`: `adapteeType.isNothing() ||
/// adapteeType.isNullableNothing()` → CONFLICT). A parameter type comes from the selected function
/// type, but the specialized invoke's RESULT is the body's own inferred type: `{ null }` selected
/// as `(Any?) -> Any?` still returns `Nothing?`, which maps to `Void` and has no adaptee.
///
/// Only a source lambda lowered through `checked_lambda` has an inferred-result entry. A lambda
/// stored in a dependency inline body already has a class-file representation and is regenerated
/// by the bytecode inliner; it does not pass through this source-lambda classification again.
/// Callable references have no inferred result in kotlinc either, so their absence here matches
/// the reference.
fn nothing_conflict(ir: &IrFile, fid: FunId, signature: &crate::types::FnSig) -> bool {
    fn is_nothing(ty: Ty) -> bool {
        matches!(ty.non_null(), Ty::Nothing | Ty::Null)
    }
    let Some(inferred_result) = ir.lambda_inferred_results.get(&fid).copied() else {
        // Current language levels select the caller-facing result as the implementation signature.
        // The frontend records that decision by omitting the pre-2.4 inferred-result fact, so this
        // backend does not need a parallel language-version option.
        return false;
    };
    signature.params.iter().copied().any(is_nothing) || is_nothing(inferred_result)
}

/// The result type used by the lambda implementation method. Before language level 2.4 checked
/// FIR records the body's inferred result; current language levels deliberately omit that fact and
/// use the selected function signature's result instead.
fn implementation_result(ir: &IrFile, fid: FunId, signature: &crate::types::FnSig) -> Ty {
    ir.lambda_inferred_results
        .get(&fid)
        .copied()
        .unwrap_or(signature.ret)
}

/// Whether the class's `FunctionN` supertype is written raw, without the generic `Signature`.
/// kotlinc's `IrTypeMapper.writeGenericType` skips the generics when
/// `hasNothingInNonContravariantPosition(supertype)` holds (`KotlinTypeMapper.kt`): an argument
/// `isNullableNothing()`, or `isNothing()` where the type parameter's variance is not `IN`.
/// `FunctionN`'s value parameters are `in` — a non-null `Nothing` there keeps the generic
/// supertype, its argument written as a star (`Function1<*Lkotlin/Unit;>;`) — while its result is
/// `out`, so a `Nothing`/`Nothing?` implementation result leaves the supertype raw. That result is
/// the pre-2.4 inferred body result when recorded, otherwise the selected function result; the raw
/// generic-signature rule is independent of why this lambda was forced to a class.
fn raw_supertype(ir: &IrFile, fid: FunId, signature: &crate::types::FnSig) -> bool {
    fn is_nothing(ty: Ty) -> bool {
        matches!(ty.non_null(), Ty::Nothing | Ty::Null)
    }
    signature
        .params
        .iter()
        .copied()
        .any(|ty| is_nothing(ty) && ty.admits_null())
        || is_nothing(implementation_result(ir, fid, signature))
}

/// Whether a value class's declared underlying type admits `null`: a nullable type, or a type
/// parameter whose bound does (`Result<T>(val a: T)` over `T : Any?`).
fn admits_null(underlying: Ty) -> bool {
    match underlying.kind() {
        TyKind::TyParam(_, bound) => admits_null(bound),
        _ => underlying.admits_null(),
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
            let capture = match identity.role {
                IrParameterRole::CapturedValue { .. } => crate::ir::IrLambdaCapture::Value {
                    source_name: identity.source_name.as_deref()?.into(),
                    inlined: identity.provenance
                        == crate::ir::IrParameterProvenance::InlinedCapture,
                },
                IrParameterRole::CapturedReceiver { ordinal } => {
                    parameter_info.captured_receivers.get(ordinal as usize)?;
                    crate::ir::IrLambdaCapture::Receiver(ordinal)
                }
                _ => return None,
            };
            let (ty, shared_holder) =
                match ir.shared_capture_parameters.get(&(fid, parameter as u32)) {
                    Some(element) => {
                        let holder = super::shared_captures::holder_ty(element);
                        let identity = holder
                            .obj_internal()
                            .expect("a shared-capture holder has an object identity");
                        (holder, Some(identity))
                    }
                    None => (function.params[parameter], None),
                };
            Some(Capture {
                capture,
                ty,
                shared_holder,
            })
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
    role: ClassRole,
) {
    let fresh_instance = ir
        .specialized_functions
        .get(&fid)
        .is_some_and(|specialization| specialization.consumed_inline_capture);
    let inline_default = role.default;
    let kotlin_lambda = role.kotlin_lambda.is_some();
    super::shared_captures::realize_lambda_capture_parameters(ir, fid, captures.len());
    let class = declare_class(ir, fid, site, signature, captures, role);
    adopt_lifted_declarations(ir, fid, class);
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
    // bridge boxes. kotlinc types it by the body's INFERRED result, which a `Nothing`/`Nothing?`
    // body pins to `Void` even when the selected function type's return is wider.
    let result = match implementation_result(ir, fid, signature) {
        inferred if matches!(inferred.non_null(), Ty::Nothing | Ty::Null) => {
            if inferred.admits_null() {
                Ty::nullable(Ty::Nothing)
            } else {
                Ty::Nothing
            }
        }
        _ => match signature.ret {
            // A numbered FunctionN class has a typed override whose generic result is boxed. Past
            // the numbered interfaces, the specialized method is not that interface slot: it
            // keeps the body's scalar result and the separately emitted packed-array bridge owns
            // boxing it into Object.
            ret if is_primitive(ret)
                && !crate::jvm::names::uses_function_n(signature.params.len()) =>
            {
                Ty::nullable(ret)
            }
            ret => ret,
        },
    };
    for &expression in &expressions {
        let IrExpr::Return(Some(value)) = ir.exprs[expression as usize] else {
            continue;
        };
        if result == Ty::Unit && matches!(ir.exprs[value as usize], IrExpr::UnitInstance) {
            ir.exprs[expression as usize] = IrExpr::Return(None);
        } else if result != signature.ret {
            // An expression body arrives as a statement-less block. Box its value itself, so an
            // erased generic result (`go { id(1) }`) is cast to the wrapper rather than unboxed
            // and boxed again.
            let mut value = value;
            while let IrExpr::Block {
                stmts,
                value: Some(inner),
            } = &ir.exprs[value as usize]
            {
                if !stmts.is_empty() {
                    break;
                }
                value = *inner;
            }
            let boxed = ir.add_expr(IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::ImplicitCoercion,
                arg: value,
                type_operand: result,
            });
            ir.exprs[expression as usize] = IrExpr::Return(Some(boxed));
        }
    }
    become_invoke(ir, fid, class, captures.len(), result);
    if (inline_default.is_some() || kotlin_lambda) && site.function_type.mentions_ty_param() {
        // The class captures the enclosing declaration's type parameters; its specialized
        // `invoke` does not redeclare them, but its method Signature still names those exact
        // identities.
        ir.signatures.insert(
            fid,
            crate::ir::IrGenericSig {
                type_params: Vec::new(),
                params: signature.params.clone(),
                ret: Some(signature.ret),
                supers: Vec::new(),
            },
        );
    }
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
    replace_value(ir, site, fresh_instance);
}

/// Move every lifted declaration whose nearest enclosing lambda is `fid` into `fid`'s class.
///
/// The relation is selected from the stable lifting sequence and path, never from a generated
/// method or class spelling. Calls already carry the selected `FunId`; only a class-static call's
/// physical owner changes after its target moves.
fn adopt_lifted_declarations(ir: &mut IrFile, fid: FunId, class: ClassId) {
    let lifted_in_scope = |candidate: &crate::fir::FirLiftingSite, scope| {
        candidate.lifted
            && candidate.path[..candidate.path.len().saturating_sub(1)]
                .iter()
                .rev()
                .find(|step| step.kind == crate::lifting_provenance::LiftingCallableKind::Lambda)
                .is_some_and(|step| step.position == scope)
    };
    let Some((sequence, site)) = ir.lifted_functions.get(&fid) else {
        return;
    };
    let Some(scope) = site.path.last().map(|step| step.position) else {
        return;
    };
    let declared = ir
        .lifted_functions
        .iter()
        .filter(|(&function, (other, candidate))| {
            function != fid && other == sequence && lifted_in_scope(candidate, scope)
        })
        .map(|(&function, _)| function)
        .collect::<Vec<_>>();
    super::lifted_declaration_ownership::move_lifted_declarations_to_class(ir, class, &declared);
}

fn replace_value(ir: &mut IrFile, site: &Site, fresh_instance: bool) {
    construct(ir, site, None, fresh_instance);
}

/// Build the class's instance at `site` and each of its copies. `constructor` gives the
/// constructor's parameter types when the class is not declared in this file.
fn construct(ir: &mut IrFile, site: &Site, constructor: Option<Vec<Ty>>, fresh_instance: bool) {
    let internal = site.class;
    for node in std::iter::once(site.node).chain(site.template_copies.iter().copied()) {
        let IrExpr::Lambda { captures, .. } = &ir.exprs[node as usize] else {
            unreachable!("a lambda class site builds a lambda value");
        };
        let captures = captures.clone();
        ir.exprs[node as usize] = if captures.is_empty() && !fresh_instance {
            IrExpr::ExternalStaticInstance {
                owner: internal,
                ty: internal,
                field: "INSTANCE".to_string(),
            }
        } else {
            IrExpr::New {
                internal,
                args: captures,
                ctor_params: constructor.clone(),
                ctor_desc: None,
                external_target: None,
                defaults: Box::new([]),
                default_prefix_count: 0,
            }
        };
        // The value's type is the class's: a consumer that needs its `FunctionN` casts it.
        ir.logical_types.insert(node, Ty::obj_name(internal));
    }
}

/// Declare the class: a field and a constructor parameter per captured value, the function type
/// it implements, and the record its emitter and the representation passes read.
fn declare_class(
    ir: &mut IrFile,
    fid: FunId,
    site: &Site,
    signature: &crate::types::FnSig,
    captures: &[Capture],
    role: ClassRole,
) -> ClassId {
    let ClassRole {
        default: inline_default,
        scope: inline_scope,
        kotlin_lambda,
    } = role;
    let mut class = crate::ir::IrClass::synthetic(site.class);
    class.superclass = crate::types::type_name(if kotlin_lambda.is_some() {
        "kotlin/jvm/internal/Lambda"
    } else {
        "java/lang/Object"
    });
    // The value is built by the inline function's `$default` stub, so that stub is the
    // enclosing method. A lambda nested in another lambda of an inline function is enclosed by that
    // lambda's `invoke`. A lambda the source names directly keeps its recorded scope.
    class.enclosure = inline_default
        .and_then(|context| context.direct_owner.or(context.parent_lambda))
        .or(inline_scope.and_then(|scope| scope.parent_lambda))
        .map(crate::ir::IrEnclosure::Function)
        .or_else(|| ir.callable_reference_enclosures.get(&site.node).copied());
    class
        .interfaces
        .push(&crate::jvm::names::function_interface_internal_name(
            signature.params.len(),
        ));
    for capture in captures {
        // The field keeps the capture's source spelling; the emitter spells its JVM name from
        // `IrLambdaClass::captures`.
        let source = match &capture.capture {
            crate::ir::IrLambdaCapture::Value { source_name, .. } => source_name.to_string(),
            crate::ir::IrLambdaCapture::Receiver(_) => "this".to_string(),
        };
        class
            .fields
            .push(IrField::new(source, capture.ty).with_is_final(true));
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
    let inline_default_owner = inline_default.and_then(|context| context.direct_owner);
    // A non-inline function's `$default` stub builds its default lambda too; it is the class's
    // enclosing method like an inline default's.
    let default_stub_owner = inline_default_owner.or_else(|| {
        kotlin_lambda
            .is_some()
            .then(|| default_argument_owner(ir, fid))
            .flatten()
    });
    if class.enclosure.is_none() {
        class.enclosure = default_stub_owner.map(crate::ir::IrEnclosure::Function);
    }
    // Whatever inline function the class is written in, a caller in another package may construct
    // it, so it is public on the JVM; only a non-private owner publishes it in the Kotlin ABI.
    let inline_owner = inline_default
        .map(|context| context.root_owner)
        .or(inline_scope.map(|scope| scope.owner));
    let public_inline = inline_owner.is_some();
    let public_inline_abi = !ir.specialized_functions.contains_key(&fid)
        && inline_owner.is_some_and(|owner| !ir.method_visibility(owner).is_private());
    let requires_reification =
        inline_owner.is_some_and(|owner| type_requires_reification(ir, owner, site.function_type));
    let extension_receiver_label = ir
        .lambda_origins
        .get(&fid)
        .and_then(|origin| origin.label.clone());
    class.lambda = Some(crate::ir::IrLambdaClass {
        // The class is constructed from other packages: a public `inline` caller inlines the
        // default, and a private one still crosses packages inside the module.
        public_inline,
        public_inline_abi,
        // Only a lambda the inline function itself declares; one nested in it is the outer
        // class's code, already mapped.
        inline_origin: inline_default_owner.is_some()
            || inline_scope.is_some_and(|scope| scope.direct),
        requires_reification,
        extension_receiver_label,
        inline_default_owner,
        default_stub_owner,
        invoke: fid,
        function_type: site.function_type,
        raw_supertype: raw_supertype(ir, fid, signature),
        captures: captures
            .iter()
            .map(|capture| capture.capture.clone())
            .collect(),
        shared_capture_holders: captures
            .iter()
            .enumerate()
            .filter_map(|(ordinal, capture)| {
                capture.shared_holder.map(|holder| {
                    (
                        u32::try_from(ordinal).expect("a lambda capture ordinal fits u32"),
                        holder,
                    )
                })
            })
            .collect(),
        captured_receivers: ir
            .fn_params
            .get(&fid)
            .expect("a realized lambda's captures were read from its parameter record")
            .captured_receivers
            .clone(),
        lifting_root: super::lifted_names::lifting_root(ir, fid),
        lifting_scope: super::lifted_names::lifting_scope(ir, fid),
        bridge: crate::ir::IrInvokeBridge::logical(signature.params.clone(), signature.ret),
        kotlin_lambda,
    });
    ir.add_class(class)
}

/// A temporary identity for a specialized lambda class. `;` cannot occur in a JVM internal name,
/// so the placeholder cannot collide with a source class or be emitted.
fn specialized_lambda_placeholder(facade: &str, function: FunId) -> TypeName {
    crate::types::type_name(&format!("{facade};specialized-lambda#{function}"))
}

/// Name each specialized lambda class from the caller's final placement.
///
/// Realization runs before lifted names exist, so the class is born under
/// [`specialized_lambda_placeholder`]. Suspend copies are named by their own pass.
pub(super) fn rename_specialized_lambda_classes(
    ir: &mut IrFile,
    facade: &str,
    modes: crate::jvm::ir_emit::LambdaModes,
) -> crate::fx_hash::HashMap<TypeName, TypeName> {
    let pending = ir
        .classes
        .iter()
        .filter_map(|class| {
            let invoke = class.lambda.as_ref()?.invoke;
            let from = class.fq_name_id();
            (from == specialized_lambda_placeholder(facade, invoke)).then_some((invoke, from))
        })
        .collect::<Vec<_>>();
    let mut all_names = crate::fx_hash::HashMap::default();
    for (function, from) in pending {
        let name =
            crate::jvm::ir_emit::lambda_class_names::specialized_name(ir, function, facade, modes)
                .expect("a specialized reified lambda retains complete caller provenance");
        let names = crate::fx_hash::HashMap::from_iter([(from, crate::types::type_name(&name))]);
        ir.remap_classifier_identities(&names);
        all_names.extend(names);
    }
    all_names
}

/// Take a lambda that is a class out of its lifting sequence: it is no lifted method, so it takes
/// no number from the lambdas and local functions lifted beside it.
fn unlift(ir: &mut IrFile, fid: FunId) {
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
    unlift(ir, fid);
    let function = &mut ir.functions[fid as usize];
    function.params.drain(..captured);
    function.param_checks = function
        .params
        .iter()
        .map(|ty| {
            super::parameter_assertions::requires_reference_guard(*ty)
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
            methods.functions().collect::<crate::fx_hash::HashSet<_>>(),
            [7, 8].into_iter().collect()
        );
    }

    #[test]
    fn appends_owned_helpers_without_ordinary_object_members() {
        let mut ir = IrFile::default();
        let mut closure = crate::ir::IrClass::synthetic(crate::types::type_name("Closure"));
        closure.lambda = Some(crate::ir::IrLambdaClass {
            public_inline: false,
            public_inline_abi: false,
            requires_reification: false,
            extension_receiver_label: None,
            inline_origin: false,
            inline_default_owner: None,
            default_stub_owner: None,
            invoke: 3,
            function_type: Ty::fun(vec![], Ty::Unit),
            raw_supertype: false,
            captures: vec![],
            shared_capture_holders: vec![],
            captured_receivers: vec![],
            lifting_root: None,
            lifting_scope: None,
            bridge: crate::ir::IrInvokeBridge::logical(vec![], Ty::Unit),
            kotlin_lambda: None,
        });
        closure.methods = vec![3, 4];
        ir.classes.push(closure);
        let mut ordinary = crate::ir::IrClass::synthetic(crate::types::type_name("OrdinaryObject"));
        ordinary.methods = vec![5];
        ir.classes.push(ordinary);
        let mut methods = LambdaMethods::default();
        methods.include_owned_methods(&ir);
        assert_eq!(
            methods.functions().collect::<crate::fx_hash::HashSet<_>>(),
            [3, 4].into_iter().collect()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn function(ir: &mut IrFile, body: Option<ExprId>) -> FunId {
        ir.add_fun(crate::ir::IrFunction {
            name: "f".to_string(),
            params: Vec::new(),
            ret: Ty::Unit,
            body,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        })
    }

    fn lambda(ir: &mut IrFile, impl_fn: FunId) -> ExprId {
        ir.add_expr(IrExpr::Lambda {
            impl_fn,
            arity: 0,
            captures: Vec::new(),
            sam: None,
            inline_body: None,
        })
    }

    #[test]
    fn the_shared_index_answers_as_each_lambdas_own_walk() {
        let mut ir = IrFile::default();
        let first = function(&mut ir, None);
        let second = function(&mut ir, None);
        let unreached = function(&mut ir, None);
        let values = vec![
            lambda(&mut ir, second),
            lambda(&mut ir, first),
            lambda(&mut ir, second),
        ];
        let body = ir.add_expr(IrExpr::Block {
            stmts: values,
            value: None,
        });
        let other_value = lambda(&mut ir, first);
        let other_body = ir.add_expr(IrExpr::Block {
            stmts: vec![other_value],
            value: None,
        });
        // Left behind in the arena by an earlier rewrite: no emitted root reaches it.
        lambda(&mut ir, unreached);
        function(&mut ir, Some(body));
        function(&mut ir, Some(other_body));

        let mut index = ReachableLambdaIndex::default();
        for every_emitted_root in [false, true] {
            for fid in [first, second, unreached] {
                assert_eq!(
                    index.reachable(&ir, fid, every_emitted_root),
                    reachable_lambdas(&ir, fid, every_emitted_root),
                );
            }
        }
        assert_eq!(index.reachable(&ir, first, true).len(), 2);
        assert_eq!(index.reachable(&ir, second, true).len(), 2);
        assert!(index.reachable(&ir, unreached, true).is_empty());
    }
}
