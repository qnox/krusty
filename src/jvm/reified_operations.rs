//! JVM realization of reified operations at declarations and call sites.
//!
//! Common IR retains checked Kotlin operations, substitutions, and exact semantic type-parameter
//! identities. For a declaration, this module realizes those operations as kotlinc-compatible
//! marker instructions before generic erasure. For a call site, it converts each checked semantic
//! substitution into either the concrete JVM classifier or the host reified parameter consumed by
//! the bytecode splicer.

use crate::fx_hash::{HashMap, HashSet};

use super::reified_arguments::ReifiedArgument;
use crate::ir::{ClassId, ExprId, FunId, IrEnclosure, IrExpr, IrFile, IrTypeOp, IrTypeParameter};
use crate::types::{stored_value_ty, Ty, TypeName};

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReifiedParameter {
    source_name: String,
    erased: TypeName,
}

fn erased_classifier(ty: Ty) -> Option<TypeName> {
    if let Some(function) = jvm_function_erasure(ty) {
        return Some(crate::types::type_name(&function));
    }
    let classifier = ty.non_null().obj_internal()?;
    Some(super::jvm_class_map::to_jvm_type_name(classifier))
}

fn reified_parameters(
    parameters: &[IrTypeParameter],
    enclosing: &[IrTypeParameter],
) -> HashMap<String, ReifiedParameter> {
    let erasures = super::generic_erasure::parameter_erasures_with(parameters, enclosing);
    parameters
        .iter()
        .filter(|parameter| parameter.reified)
        .filter_map(|parameter| {
            let erased = erased_classifier(*erasures.get(&parameter.semantic_name)?)?;
            Some((
                parameter.semantic_name.clone(),
                ReifiedParameter {
                    source_name: parameter.name.clone(),
                    erased,
                },
            ))
        })
        .collect()
}

fn parameter(ty: Ty, parameters: &HashMap<String, ReifiedParameter>) -> Option<&ReifiedParameter> {
    let Ty::TyParam(identity, _) = ty.non_null() else {
        return None;
    };
    parameters.get(identity)
}

/// kotlinc's `extractReificationArgument` for the element type of an array a reified body creates:
/// the reified parameter the element type reaches through nested arrays, with the marker argument
/// that names it (`[` per array level, the source spelling, `?` when the element is marked
/// nullable). `None` when the element is no reified parameter of this file, or reaches one only
/// through another classifier or a star projection.
fn new_array_reification(
    array_type: Ty,
    parameters: &HashMap<String, ReifiedParameter>,
) -> Option<(String, String)> {
    let projected = |ty: Ty| match ty {
        Ty::OutProjection(inner) | Ty::InProjection(inner) => Some(*inner),
        Ty::StarProjection(_) => None,
        element => Some(element),
    };
    let element = projected(array_type.non_null().array_elem()?)?;
    let nullable = element.is_nullable();
    let mut depth = 0;
    let mut current = element;
    while current.non_null().is_reference_array() {
        depth += 1;
        current = projected(current.non_null().array_elem()?)?;
    }
    let Ty::TyParam(identity, _) = current.non_null() else {
        return None;
    };
    let parameter = parameters.get(identity)?;
    let suffix = if nullable { "?" } else { "" };
    Some((
        identity.to_owned(),
        format!("{}{}{suffix}", "[".repeat(depth), parameter.source_name),
    ))
}

/// The splice operands for the checked reified substitutions attached to one call. Common IR
/// retains semantic [`Ty`] values; the conversion to physical classifier names belongs here at the
/// backend boundary. A `Unit` value uses its stored classifier form, just like every other value
/// that a reified type-bearing instruction materializes. A substitution that is itself a reified
/// type parameter of a declaration in this file has no class yet: that declaration is a reified
/// inline body, so the callee's marker is forwarded under the declaration's own parameter name.
///
/// `render` spells a type as kotlinc does in `null cannot be cast to non-null type …`, which a
/// non-null reified `as` throws.
pub(super) fn splice_type_map(
    ir: &IrFile,
    expression: ExprId,
    render: &dyn Fn(Ty) -> String,
) -> HashMap<String, ReifiedArgument> {
    let Some(substitutions) = ir.reified_call_subst.get(&expression) else {
        return HashMap::default();
    };
    let forwarded = |ty: Ty| {
        let Ty::TyParam(identity, _) = ty.non_null() else {
            return None;
        };
        ir.signatures
            .values()
            .flat_map(|signature| &signature.type_params)
            .find(|parameter| parameter.reified && parameter.semantic_name == identity)
            .map(|parameter| ReifiedArgument::Forwarded {
                name: parameter.name.clone(),
                nullable: ty.is_nullable(),
            })
    };
    substitutions
        .iter()
        .filter_map(|(name, ty)| {
            let argument = forwarded(*ty).or_else(|| {
                Some(ReifiedArgument::Class {
                    internal: reified_class_internal(*ty)?,
                    nullable: ty.is_nullable(),
                    intrinsic: ir.type_check_role(*ty),
                    rendered: render(ty.non_null()),
                })
            })?;
            Some((name.clone(), argument))
        })
        .collect()
}

/// The JVM class a reified argument's type-bearing instruction names: a function type's
/// `FunctionN` (`Function{N+1}` when it suspends), an array's descriptor, otherwise the stored
/// classifier's mapped class.
fn reified_class_internal(ty: Ty) -> Option<String> {
    if let Some(function) = jvm_function_erasure(ty) {
        return Some(function);
    }
    let value = ty.non_null();
    if value.is_array() {
        return Some(super::names::instanceof_internal_name(value));
    }
    let internal = stored_value_ty(ty).kotlin_class_internal()?.render();
    Some(super::jvm_class_map::to_jvm_internal(&internal).to_owned())
}

/// JVM function interface a function type erases to. A suspend function counts its continuation,
/// so `suspend () -> Unit` and `SuspendFunction0` are both `Function1`.
fn jvm_function_erasure(ty: Ty) -> Option<String> {
    let value = ty.non_null();
    let (arity, suspend) = if let Ty::Fun(signature) = value {
        (signature.params.len(), signature.suspend)
    } else {
        let function = super::function_classifiers::classifier(value.obj_internal()?)?;
        if !function.is_suspend() || function.is_reflective() {
            return None;
        }
        (function.arity(), true)
    };
    Some(super::names::function_interface_internal_name(
        arity + usize::from(suspend),
    ))
}

/// Everything a splice of the call `expression` needs to specialize its dependency's reified
/// markers: the JVM classes of its reified arguments, and for `typeOf` markers each argument's
/// realization (plain and nullable) built in this host file.
pub(super) fn splice_arguments(
    ir: &IrFile,
    expression: ExprId,
    facade: &str,
    recursive_type_of: bool,
    render: &dyn Fn(Ty) -> String,
) -> super::reified_arguments::ReifiedArguments {
    let classes = splice_type_map(ir, expression, render);
    let mut type_of = HashMap::default();
    if let Some(substitutions) = ir.reified_call_subst.get(&expression) {
        let parameters = super::type_of::TypeParameters::new(ir, facade);
        for (name, ty) in substitutions {
            for (argument, ty) in [(name.clone(), *ty), (format!("{name}?"), Ty::nullable(*ty))] {
                let mut realization = Vec::new();
                if super::type_of::generate(ty, &parameters, recursive_type_of, &mut realization)
                    .is_ok()
                {
                    type_of.insert(argument, realization);
                }
            }
        }
    }
    super::reified_arguments::ReifiedArguments { classes, type_of }
}

fn collect_reified_parameters(ir: &IrFile) -> HashMap<String, ReifiedParameter> {
    let signatures = ir
        .signatures
        .iter()
        .map(|(&function, signature)| (function, signature.type_params.clone()))
        .collect::<Vec<_>>();
    let mut parameters = HashMap::default();
    for (function, type_params) in signatures {
        let enclosing = super::generic_erasure::bound_type_parameters(ir, function);
        for (identity, parameter) in reified_parameters(&type_params, &enclosing) {
            if let Some(previous) = parameters.insert(identity, parameter.clone()) {
                assert_eq!(
                    previous, parameter,
                    "one semantic reified parameter has consistent declaration facts"
                );
            }
        }
    }
    parameters
}

fn owns_reified_parameter(ir: &IrFile, function: FunId) -> bool {
    ir.signatures.get(&function).is_some_and(|signature| {
        signature
            .type_params
            .iter()
            .any(|parameter| parameter.reified)
    })
}

fn in_reified_body(ir: &IrFile, enclosure: Option<IrEnclosure>) -> bool {
    super::ir_emit::enclosure::enclosing_functions(ir, enclosure)
        .any(|(function, _)| owns_reified_parameter(ir, function))
}

/// Declarations whose bodies may execute a reification marker: a signature that owns a reified
/// parameter, any function declared at any depth in such a body (an anonymous or local object's
/// member, which code inlining the declaration regenerates with the call site's classes, as kotlinc
/// does), and the exact lambda implementations `realize` already normalizes.
fn reified_marker_functions(
    ir: &IrFile,
    lambdas: &super::lambda_classes::LambdaMethods,
) -> HashSet<FunId> {
    let mut functions = lambdas.functions().collect::<HashSet<_>>();
    functions.extend(
        (0..ir.functions.len() as FunId)
            .filter(|&function| in_reified_body(ir, Some(IrEnclosure::Function(function)))),
    );
    functions
}

/// The class a class is declared in, when it is declared in one's code.
fn outer_class(ir: &IrFile, class: ClassId) -> Option<ClassId> {
    let mut enclosure = ir.classes[class as usize].enclosure;
    loop {
        match enclosure? {
            IrEnclosure::Lambda(function) => {
                enclosure = ir.lambda_enclosures.get(&function).copied();
            }
            IrEnclosure::Constructor { class, .. }
            | IrEnclosure::ClassInitializer(class)
            | IrEnclosure::Classifier(class) => return Some(class),
            scope => {
                let (_, owner) =
                    super::ir_emit::enclosure::enclosing_functions(ir, Some(scope)).next()?;
                return ir.class_id_by_name(owner?);
            }
        }
    }
}

/// Record the classes declared in a reified body whose code executes a marker: constructing one
/// calls `needClassReification`, which tells an inliner to regenerate the class. Like kotlinc's
/// codegen merging a nested class's reified usages into its parent's, an enclosing class declared
/// in the same body is recorded too.
fn record_reifying_classes(ir: &mut IrFile, classes: impl IntoIterator<Item = ClassId>) {
    for class in classes {
        let mut current = Some(class);
        let mut seen = HashSet::default();
        while let Some(class) = current {
            if !seen.insert(class) || !in_reified_body(ir, ir.classes[class as usize].enclosure) {
                break;
            }
            // Common inline lowering may already have recorded the directly constructed class.
            // Keep walking: a marker in a nested class also makes every enclosing generated class
            // part of the reified copy, and prior membership is not proof that those owners were
            // recorded.
            ir.reified_anonymous_declarations.insert(class);
            current = outer_class(ir, class);
        }
    }
}

/// Body and declaration-owned default expressions of `functions`. A default is not a child of the
/// body, so a catch written there is its own root.
fn marker_roots<'a>(ir: &IrFile, functions: impl IntoIterator<Item = &'a FunId>) -> Vec<ExprId> {
    let mut roots = Vec::new();
    for &function in functions {
        if let Some(body) = ir
            .functions
            .get(function as usize)
            .and_then(|function| function.body)
        {
            roots.push(body);
        }
        let Some(defaults) = ir
            .fn_params
            .get(&function)
            .and_then(|info| info.defaults.as_ref())
        else {
            continue;
        };
        roots.extend(defaults.iter().copied().flatten());
    }
    roots
}

/// Record catches that still carry a declaration-owned reified parameter across the complete exact
/// emitted-root domain: selected functions and every class-owned root in a reified enclosure.
///
/// The plan is the declaration's source spelling, keyed by the `try` expression and parallel to
/// its catches. Later passes may move a catch onto a fresh `try`, so emission rebuilds the plan
/// from the same declaration facts. A catch whose type is already a class records nothing.
fn record_reified_catches(
    ir: &mut IrFile,
    parameters: &HashMap<String, ReifiedParameter>,
    functions: &HashSet<FunId>,
) {
    ir.reified_catch_markers.clear();
    let mut visited = HashSet::default();
    let mut roots = marker_roots(ir, functions);
    for class in &ir.classes {
        if in_reified_body(ir, class.enclosure) {
            roots.extend(class_marker_roots(ir, class));
        }
    }
    for root in roots {
        let mut pending = vec![root];
        while let Some(expression) = pending.pop() {
            if !visited.insert(expression) {
                continue;
            }
            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
            let IrExpr::Try { catches, .. } = ir.expr(expression) else {
                continue;
            };
            let markers = catches
                .iter()
                .map(|catch| parameter(catch.ty, parameters).map(|found| found.source_name.clone()))
                .collect::<Vec<_>>();
            if markers.iter().any(Option::is_some) {
                ir.reified_catch_markers.insert(expression, markers);
            }
        }
    }
}

/// Rebuild the reified-catch plan from declaration parameters. Emission calls this after suspend
/// lowering, which can move a catch onto a new `try`.
pub(super) fn record_catch_markers(
    ir: &mut IrFile,
    lambdas: &super::lambda_classes::LambdaMethods,
) {
    let parameters = collect_reified_parameters(ir);
    let functions = reified_marker_functions(ir, lambdas);
    record_reified_catches(ir, &parameters, &functions);
}

fn realize_expression_dag(
    ir: &mut IrFile,
    root: ExprId,
    parameters: &HashMap<String, ReifiedParameter>,
    marker_identities: &[HashSet<String>],
) -> HashSet<String> {
    let mut used_parameters = HashSet::default();
    let mut pending = vec![root];
    let mut visited = HashSet::default();
    while let Some(expression) = pending.pop() {
        if !visited.insert(expression) {
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        used_parameters.extend(marker_identities[expression as usize].iter().cloned());
        if let IrExpr::NewArray { array_type, .. } | IrExpr::Vararg { array_type, .. } =
            ir.expr(expression)
        {
            if let Some((_, marker)) = new_array_reification(*array_type, parameters) {
                ir.reified_array_markers.insert(expression, marker);
            }
        }
        let replacement = match ir.expr(expression).clone() {
            IrExpr::KClassLiteral {
                classifier: Some(classifier),
                value: None,
                ..
            } => parameter(classifier, parameters).map(|parameter| IrExpr::ReifiedClassMarker {
                name: parameter.source_name.clone(),
                erased: parameter.erased,
                kclass: true,
            }),
            IrExpr::TypeOp {
                op,
                arg,
                type_operand,
            } => parameter(type_operand, parameters).and_then(|parameter| {
                let (cast, negated) = match op {
                    IrTypeOp::InstanceOf => (false, false),
                    IrTypeOp::NotInstanceOf => (false, true),
                    IrTypeOp::Cast | IrTypeOp::CastNonNull => (true, false),
                    IrTypeOp::SafeCast | IrTypeOp::ImplicitCoercion => return None,
                };
                // kotlinc's `ReificationArgument.asString`: a marked-nullable operand names `T?`,
                // which the call site specializes to a test that also accepts `null`.
                let name = if type_operand.is_nullable() {
                    format!("{}?", parameter.source_name)
                } else {
                    parameter.source_name.clone()
                };
                Some(IrExpr::ReifiedTypeOp {
                    cast,
                    negated,
                    arg,
                    name,
                    erased: parameter.erased,
                })
            }),
            _ => None,
        };
        if let Some(replacement) = replacement {
            ir.exprs[expression as usize] = replacement;
        }
    }
    used_parameters
}

/// Whether this exact semantic type occurrence carries a declaration-owned reified parameter that
/// a JVM marker can consume. Do not inspect a non-reified parameter's bound: `typeOf<S>()` describes
/// `S` as a runtime type parameter even when one of its bounds mentions a different reified `T`.
fn collect_type_reified_parameters(
    ty: Ty,
    parameters: &HashMap<String, ReifiedParameter>,
    found: &mut HashSet<String>,
) {
    match ty {
        Ty::TyParam(identity, _) => {
            if parameters.contains_key(identity) {
                found.insert(identity.to_owned());
            }
        }
        Ty::Obj(_, arguments) | Ty::Intersection(arguments) => arguments
            .iter()
            .copied()
            .for_each(|argument| collect_type_reified_parameters(argument, parameters, found)),
        Ty::Fun(signature) => {
            signature.params.iter().copied().for_each(|parameter| {
                collect_type_reified_parameters(parameter, parameters, found)
            });
            collect_type_reified_parameters(signature.ret, parameters, found);
        }
        Ty::Nullable(inner)
        | Ty::PlatformNullable(inner)
        | Ty::InProjection(inner)
        | Ty::OutProjection(inner)
        | Ty::DefinitelyNotNull(inner) => {
            collect_type_reified_parameters(*inner, parameters, found);
        }
        // A star's readable bound is semantic information, but `typeOf<C<*>>()` emits
        // `KTypeProjection.star` and no operation for that bound.
        Ty::StarProjection(_) | Ty::Unit | Ty::Null | Ty::Nothing | Ty::Error | Ty::Pending => {}
    }
}

/// The declaration-qualified parameters whose JVM markers this exact expression emits. This plan
/// is built before any expression is replaced with its text-bearing marker form, so shared DAG
/// roots retain semantic identity even when an earlier root has already been realized.
fn expression_reified_parameters(
    ir: &IrFile,
    expression: ExprId,
    parameters: &HashMap<String, ReifiedParameter>,
) -> HashSet<String> {
    let mut found = HashSet::default();
    if let Some(substitutions) = ir.reified_call_subst.get(&expression) {
        for (_, ty) in substitutions {
            collect_type_reified_parameters(*ty, parameters, &mut found);
        }
    }
    match ir.expr(expression) {
        IrExpr::KClassLiteral {
            classifier: Some(classifier),
            value: None,
            ..
        }
        | IrExpr::TypeOp {
            type_operand: classifier,
            ..
        } => collect_type_reified_parameters(*classifier, parameters, &mut found),
        IrExpr::Try { catches, .. } => {
            for catch in catches {
                collect_type_reified_parameters(catch.ty, parameters, &mut found);
            }
        }
        IrExpr::NewArray { array_type, .. } | IrExpr::Vararg { array_type, .. } => {
            found.extend(
                new_array_reification(*array_type, parameters).map(|(identity, _)| identity),
            );
        }
        IrExpr::Call {
            callee:
                crate::ir::Callee::Intrinsic {
                    operation:
                        crate::ir::IrIntrinsic::TypeOf { ty: classifier }
                        | crate::ir::IrIntrinsic::EnumValueOf { classifier }
                        | crate::ir::IrIntrinsic::EnumEntries { classifier },
                    ..
                },
            ..
        } => collect_type_reified_parameters(*classifier, parameters, &mut found),
        _ => {}
    }
    found
}

/// All IR-owned sources that emit `Intrinsics.reifiedOperationMarker` in this backend. Ownership of
/// a generated class is derived from this one plan, so adding a marker producer cannot silently
/// leave construction without `needClassReification`.
#[cfg(test)]
fn expression_emits_reified_marker(
    ir: &IrFile,
    expression: ExprId,
    parameters: &HashMap<String, ReifiedParameter>,
) -> bool {
    if !expression_reified_parameters(ir, expression, parameters).is_empty() {
        return true;
    }
    match ir.expr(expression) {
        IrExpr::ReifiedClassMarker { .. } | IrExpr::ReifiedTypeOp { .. } => true,
        _ => false,
    }
}

fn member_uses_enclosing_reified_parameter(
    ir: &IrFile,
    function: FunId,
    used_parameters: &HashSet<String>,
) -> bool {
    let owned = ir
        .signatures
        .get(&function)
        .into_iter()
        .flat_map(|signature| &signature.type_params)
        .filter(|parameter| parameter.reified)
        .map(|parameter| parameter.semantic_name.as_str())
        .collect::<HashSet<_>>();
    used_parameters
        .iter()
        .any(|identity| !owned.contains(identity.as_str()))
}

/// Expression roots physically emitted as part of one class rather than one [`IrFunction`].
/// Keeping this inventory beside the marker plan makes constructors and property/enum
/// initialization participate in the same ownership decision as methods and accessors.
fn class_marker_roots(ir: &IrFile, class: &crate::ir::IrClass) -> Vec<ExprId> {
    let mut roots = class.init_body.into_iter().collect::<Vec<_>>();
    roots.extend(ir.companion_clinit_body(class.fq_name));
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
        roots.extend(constructor.defaults.iter().copied().flatten());
        roots.extend(constructor.delegate_prelude.iter().copied());
        roots.extend(constructor.delegate_args.iter().copied());
    }
    for entry in &class.enum_entries {
        roots.extend(entry.argument_prelude.iter().copied());
        roots.extend(entry.args.iter().copied());
    }
    roots.extend(
        ir.statics
            .iter()
            .filter(|property| property.owner == Some(class.fq_name))
            .filter_map(|property| property.init),
    );
    roots
}

pub(super) fn realize(ir: &mut IrFile, lambdas: &super::lambda_classes::LambdaMethods) {
    // Type operands already carry declaration-qualified semantic parameter identities. Realizing a
    // closure can replace its Lambda node with a class value, but cannot change those identities.
    // Normalize the exact declaration/closure method domain from that checked map rather than
    // rediscovering implementations through expression nodes already consumed by representation.
    // An object declared in a reified body executes the marker too: inlining regenerates its class.
    let parameters = collect_reified_parameters(ir);
    let functions = reified_marker_functions(ir, lambdas);
    ir.reified_array_markers.clear();
    let marker_identities = (0..ir.exprs.len() as ExprId)
        .map(|expression| expression_reified_parameters(ir, expression, &parameters))
        .collect::<Vec<_>>();
    let mut reifying = Vec::new();
    for &function in &functions {
        let mut used_parameters = HashSet::default();
        for root in marker_roots(ir, [&function]) {
            used_parameters.extend(realize_expression_dag(
                ir,
                root,
                &parameters,
                &marker_identities,
            ));
        }
        let owner = super::ir_emit::enclosure::enclosing_functions(
            ir,
            Some(IrEnclosure::Function(function)),
        )
        .next()
        .and_then(|(_, owner)| owner);
        if member_uses_enclosing_reified_parameter(ir, function, &used_parameters) {
            reifying.extend(owner.and_then(|owner| ir.class_id_by_name(owner)));
        }
    }
    for class in 0..ir.classes.len() as ClassId {
        let declaration = &ir.classes[class as usize];
        if !in_reified_body(ir, declaration.enclosure) {
            continue;
        }
        let roots = class_marker_roots(ir, declaration);
        let mut used_parameters = HashSet::default();
        for root in roots {
            used_parameters.extend(realize_expression_dag(
                ir,
                root,
                &parameters,
                &marker_identities,
            ));
        }
        if !used_parameters.is_empty() {
            reifying.push(class);
        }
    }
    record_reifying_classes(ir, reifying);
    record_reified_catches(ir, &parameters, &functions);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrCatch, IrFunction, IrGenericSig};

    fn reified_signature(identity: &str) -> IrGenericSig {
        IrGenericSig {
            type_params: vec![IrTypeParameter {
                name: "T".to_owned(),
                semantic_name: identity.to_owned(),
                bounds: vec![(Ty::obj("kotlin/Any"), false)],
                variance: Default::default(),
                reified: true,
            }],
            params: vec![],
            ret: None,
            supers: vec![],
        }
    }

    fn function(ir: &mut IrFile, body: ExprId, identity: &str) {
        ir.functions.push(IrFunction {
            name: "useT".to_owned(),
            params: vec![],
            ret: Ty::obj("kotlin/Any"),
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![],
        });
        ir.signatures.insert(0, reified_signature(identity));
    }

    #[test]
    fn splice_type_map_uses_the_stored_unit_classifier() {
        let expression = 7;
        let mut ir = IrFile::default();
        ir.reified_call_subst.insert(
            expression,
            vec![("T".to_owned(), Ty::Unit), ("R".to_owned(), Ty::String)],
        );

        assert_eq!(
            splice_type_map(&ir, expression, &|_| String::new()),
            HashMap::from_iter([
                (
                    "T".to_owned(),
                    ReifiedArgument::Class {
                        internal: "kotlin/Unit".to_owned(),
                        nullable: false,
                        intrinsic: None,
                        rendered: String::new(),
                    }
                ),
                (
                    "R".to_owned(),
                    ReifiedArgument::Class {
                        internal: "java/lang/String".to_owned(),
                        nullable: false,
                        intrinsic: None,
                        rendered: String::new(),
                    }
                ),
            ])
        );
    }

    #[test]
    fn splice_type_map_forwards_a_reified_parameter_of_the_host() {
        let identity = "T@only";
        let expression = 7;
        let mut ir = IrFile::default();
        ir.signatures.insert(0, reified_signature(identity));
        let parameter = Ty::ty_param(identity, Ty::obj("kotlin/Any"));
        ir.reified_call_subst.insert(
            expression,
            vec![
                ("R".to_owned(), parameter),
                ("S".to_owned(), Ty::nullable(parameter)),
            ],
        );

        assert_eq!(
            splice_type_map(&ir, expression, &|_| String::new()),
            HashMap::from_iter([
                (
                    "R".to_owned(),
                    ReifiedArgument::Forwarded {
                        name: "T".to_owned(),
                        nullable: false,
                    },
                ),
                (
                    "S".to_owned(),
                    ReifiedArgument::Forwarded {
                        name: "T".to_owned(),
                        nullable: true,
                    },
                ),
            ])
        );
    }

    #[test]
    fn marker_plan_covers_type_of_catches_and_forwarded_calls() {
        let identity = "T@owner";
        let parameter_ty = Ty::ty_param(identity, Ty::obj("kotlin/Any"));
        let parameters = HashMap::from_iter([(
            identity.to_owned(),
            ReifiedParameter {
                source_name: "T".to_owned(),
                erased: crate::types::type_name("java/lang/Object"),
            },
        )]);
        let mut ir = IrFile::default();
        let type_of = ir.add_expr(IrExpr::Call {
            callee: crate::ir::Callee::Intrinsic {
                operation: crate::ir::IrIntrinsic::TypeOf {
                    ty: Ty::obj_args("kotlin/collections/List", &[parameter_ty]),
                },
                ret: Ty::obj("kotlin/reflect/KType"),
            },
            dispatch_receiver: None,
            args: Vec::new(),
        });
        let thrown = ir.add_expr(IrExpr::UnitInstance);
        let handler = ir.add_expr(IrExpr::UnitInstance);
        let catch = ir.add_expr(IrExpr::Try {
            body: thrown,
            catches: vec![IrCatch {
                var: 1,
                binding: None,
                ty: parameter_ty,
                body: handler,
                line: None,
            }],
            finally: None,
            result: Ty::Unit,
        });
        let forwarded = ir.add_expr(IrExpr::UnitInstance);
        ir.reified_call_subst.insert(
            forwarded,
            vec![(
                "R".to_owned(),
                Ty::obj_args("kotlin/collections/List", &[parameter_ty]),
            )],
        );
        let enum_entries = ir.add_expr(IrExpr::Call {
            callee: crate::ir::Callee::Intrinsic {
                operation: crate::ir::IrIntrinsic::EnumEntries {
                    classifier: parameter_ty,
                },
                ret: Ty::obj("kotlin/enums/EnumEntries"),
            },
            dispatch_receiver: None,
            args: Vec::new(),
        });
        let star = ir.add_expr(IrExpr::Call {
            callee: crate::ir::Callee::Intrinsic {
                operation: crate::ir::IrIntrinsic::TypeOf {
                    ty: Ty::obj_args(
                        "kotlin/collections/List",
                        &[Ty::star_projection(parameter_ty)],
                    ),
                },
                ret: Ty::obj("kotlin/reflect/KType"),
            },
            dispatch_receiver: None,
            args: Vec::new(),
        });

        assert!(expression_emits_reified_marker(&ir, type_of, &parameters));
        assert!(expression_emits_reified_marker(&ir, catch, &parameters));
        assert!(expression_emits_reified_marker(&ir, forwarded, &parameters));
        assert!(expression_emits_reified_marker(
            &ir,
            enum_entries,
            &parameters
        ));
        assert!(!expression_emits_reified_marker(&ir, star, &parameters));
    }

    #[test]
    fn a_pre_recorded_nested_class_still_records_its_reified_owner_chain() {
        let identity = "T@owner";
        let mut ir = IrFile::default();
        let body = ir.add_expr(IrExpr::UnitInstance);
        function(&mut ir, body, identity);
        let mut outer = crate::ir::IrClass::synthetic(crate::types::type_name("Owner$1"));
        outer.enclosure = Some(IrEnclosure::Function(0));
        let mut inner = crate::ir::IrClass::synthetic(crate::types::type_name("Owner$1$Inner"));
        inner.enclosure = Some(IrEnclosure::Classifier(0));
        ir.classes.extend([outer, inner]);
        ir.reified_anonymous_declarations.insert(1);

        record_reifying_classes(&mut ir, [1]);

        assert_eq!(
            ir.reified_anonymous_declarations,
            HashSet::from_iter([0, 1])
        );
    }

    #[test]
    fn a_member_marks_its_class_only_for_an_enclosing_reified_identity() {
        let outer_identity = "T@outer";
        let own_identity = "T@member";
        let mixed_identity = "R@member";
        let mut ir = IrFile::default();
        let unit = ir.add_expr(IrExpr::UnitInstance);
        function(&mut ir, unit, outer_identity);

        let own_operand = ir.add_expr(IrExpr::GetValue(0));
        let own_marker = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::InstanceOf,
            arg: own_operand,
            type_operand: Ty::ty_param(own_identity, Ty::obj("kotlin/Any")),
        });
        let own_class = crate::types::type_name("OwnMarker$1");
        let mut own_declaration = crate::ir::IrClass::synthetic(own_class);
        own_declaration.enclosure = Some(IrEnclosure::Function(0));
        own_declaration.methods.push(1);
        ir.classes.push(own_declaration);
        ir.functions.push(IrFunction {
            name: "ownMarker".to_owned(),
            params: vec![],
            ret: Ty::Boolean,
            body: Some(own_marker),
            is_static: false,
            dispatch_receiver: Some(own_class),
            param_checks: vec![],
        });
        ir.signatures.insert(1, reified_signature(own_identity));

        let outer_operand = ir.add_expr(IrExpr::GetValue(0));
        let outer_marker = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::InstanceOf,
            arg: outer_operand,
            type_operand: Ty::ty_param(outer_identity, Ty::obj("kotlin/Any")),
        });
        let mixed_class = crate::types::type_name("OuterMarker$1");
        let mut mixed_declaration = crate::ir::IrClass::synthetic(mixed_class);
        mixed_declaration.enclosure = Some(IrEnclosure::Function(0));
        mixed_declaration.methods.push(2);
        ir.classes.push(mixed_declaration);
        ir.functions.push(IrFunction {
            name: "outerMarker".to_owned(),
            params: vec![],
            ret: Ty::Boolean,
            body: Some(outer_marker),
            is_static: false,
            dispatch_receiver: Some(mixed_class),
            param_checks: vec![],
        });
        ir.signatures.insert(2, reified_signature(mixed_identity));

        realize(&mut ir, &Default::default());

        assert!(matches!(ir.expr(own_marker), IrExpr::ReifiedTypeOp { .. }));
        assert!(matches!(
            ir.expr(outer_marker),
            IrExpr::ReifiedTypeOp { .. }
        ));
        assert_eq!(ir.reified_anonymous_declarations, HashSet::from_iter([1]));
    }

    #[test]
    fn a_class_initializer_catch_uses_the_same_reified_marker_plan() {
        let identity = "E@owner";
        let parameter_ty = Ty::ty_param(identity, Ty::obj("kotlin/Throwable"));
        let mut ir = IrFile::default();
        let thrown = ir.add_expr(IrExpr::UnitInstance);
        let handler = ir.add_expr(IrExpr::UnitInstance);
        let catch = ir.add_expr(IrExpr::Try {
            body: thrown,
            catches: vec![IrCatch {
                var: 1,
                binding: None,
                ty: parameter_ty,
                body: handler,
                line: None,
            }],
            finally: None,
            result: Ty::Unit,
        });
        function(&mut ir, thrown, identity);
        let mut class = crate::ir::IrClass::synthetic(crate::types::type_name("Owner$1"));
        class.enclosure = Some(IrEnclosure::Function(0));
        class.init_body = Some(catch);
        ir.classes.push(class);

        realize(&mut ir, &Default::default());

        assert_eq!(
            ir.reified_catch_markers.get(&catch),
            Some(&vec![Some("T".to_owned())])
        );
        assert_eq!(ir.reified_anonymous_declarations, HashSet::from_iter([0]));
    }

    #[test]
    fn realizes_a_reified_class_literal_with_jvm_erasure() {
        let identity = "T@useT";
        let mut ir = IrFile::default();
        let body = ir.add_expr(IrExpr::KClassLiteral {
            classifier: Some(Ty::ty_param(identity, Ty::obj("kotlin/Any"))),
            value: None,
            type_argument: true,
        });
        function(&mut ir, body, identity);

        realize(&mut ir, &Default::default());

        assert!(matches!(
            ir.expr(body),
            IrExpr::ReifiedClassMarker {
                name,
                erased,
                kclass: true,
            } if name == "T" && erased.matches("java/lang/Object")
        ));
    }

    #[test]
    fn realizes_a_reified_instance_test_without_touching_its_operand() {
        let identity = "T@useT";
        let mut ir = IrFile::default();
        let operand = ir.add_expr(IrExpr::GetValue(0));
        let body = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::InstanceOf,
            arg: operand,
            type_operand: Ty::ty_param(identity, Ty::obj("kotlin/Any")),
        });
        function(&mut ir, body, identity);

        realize(&mut ir, &Default::default());

        assert!(matches!(
            ir.expr(body),
            IrExpr::ReifiedTypeOp {
                cast: false,
                negated: false,
                arg,
                name,
                erased,
            } if *arg == operand && name == "T" && erased.matches("java/lang/Object")
        ));
    }

    #[test]
    fn realized_methods_keep_checked_reification_without_lambda_nodes() {
        let identity = "T@declaration";
        let mut ir = IrFile::default();
        let declaration = ir.add_expr(IrExpr::UnitInstance);
        function(&mut ir, declaration, identity);
        let argument = ir.add_expr(IrExpr::GetValue(0));
        let source = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: argument,
            type_operand: Ty::ty_param(identity, Ty::obj("kotlin/Any")),
        });
        let concrete_copy = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: argument,
            type_operand: Ty::String,
        });
        let ordinary = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: argument,
            type_operand: Ty::ty_param("T@ordinary", Ty::obj("kotlin/Any")),
        });
        let erased_object_member = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: argument,
            type_operand: Ty::ty_param(identity, Ty::obj("kotlin/Any")),
        });
        for body in [source, concrete_copy, ordinary, erased_object_member] {
            let mut implementation = ir.functions[0].clone();
            implementation.name = "invoke".to_owned();
            implementation.body = Some(body);
            ir.functions.push(implementation);
        }

        let lambdas = super::super::lambda_classes::LambdaMethods::for_test([1, 2, 3]);
        realize(&mut ir, &lambdas);

        assert!(matches!(
            ir.expr(source),
            IrExpr::ReifiedTypeOp { cast: true, name, .. } if name == "T"
        ));
        assert!(matches!(
            ir.expr(concrete_copy),
            IrExpr::TypeOp {
                type_operand: Ty::String,
                ..
            }
        ));
        assert!(matches!(ir.expr(ordinary), IrExpr::TypeOp { .. }));
        assert!(matches!(
            ir.expr(erased_object_member),
            IrExpr::TypeOp { .. }
        ));
    }

    #[test]
    fn records_a_reified_catch_from_the_declaration_source_name() {
        let identity = "E@eval";
        let mut ir = IrFile::default();
        let thrown = ir.add_expr(IrExpr::UnitInstance);
        let reified_handler = ir.add_expr(IrExpr::UnitInstance);
        let ordinary_handler = ir.add_expr(IrExpr::UnitInstance);
        let reified = ir.add_expr(IrExpr::Try {
            body: thrown,
            catches: vec![
                IrCatch {
                    var: 1,
                    binding: None,
                    ty: Ty::ty_param(identity, Ty::obj("kotlin/Throwable")),
                    body: reified_handler,
                    line: Some(4),
                },
                IrCatch {
                    var: 2,
                    binding: None,
                    ty: Ty::obj("kotlin/Throwable"),
                    body: ordinary_handler,
                    line: None,
                },
            ],
            finally: None,
            result: Ty::Unit,
        });
        let plain_handler = ir.add_expr(IrExpr::UnitInstance);
        let plain = ir.add_expr(IrExpr::Try {
            body: thrown,
            catches: vec![IrCatch {
                var: 3,
                binding: None,
                ty: Ty::obj("kotlin/Throwable"),
                body: plain_handler,
                line: None,
            }],
            finally: None,
            result: Ty::Unit,
        });
        ir.functions.push(IrFunction {
            name: "eval".to_owned(),
            params: vec![],
            ret: Ty::Unit,
            body: Some(reified),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![],
        });
        ir.functions.push(IrFunction {
            name: "other".to_owned(),
            params: vec![],
            ret: Ty::Unit,
            body: Some(plain),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![],
        });
        ir.signatures.insert(
            0,
            IrGenericSig {
                type_params: vec![IrTypeParameter {
                    name: "E".to_owned(),
                    semantic_name: identity.to_owned(),
                    bounds: vec![(Ty::obj("kotlin/Throwable"), false)],
                    variance: Default::default(),
                    reified: true,
                }],
                params: vec![],
                ret: None,
                supers: vec![],
            },
        );

        realize(&mut ir, &Default::default());

        assert_eq!(
            ir.reified_catch_markers.get(&reified),
            Some(&vec![Some("E".to_owned()), None])
        );
        assert!(!ir.reified_catch_markers.contains_key(&plain));
    }

    fn reified_catch(ir: &mut IrFile, identity: &str) -> ExprId {
        let thrown = ir.add_expr(IrExpr::UnitInstance);
        let handler = ir.add_expr(IrExpr::UnitInstance);
        ir.add_expr(IrExpr::Try {
            body: thrown,
            catches: vec![IrCatch {
                var: 1,
                binding: None,
                ty: Ty::ty_param(identity, Ty::obj("kotlin/Throwable")),
                body: handler,
                line: None,
            }],
            finally: None,
            result: Ty::Unit,
        })
    }

    #[test]
    fn an_erased_object_member_catch_is_not_marked() {
        let identity = "E@eval";
        let mut ir = IrFile::default();
        let declaration = reified_catch(&mut ir, identity);
        let lambda = reified_catch(&mut ir, identity);
        let erased_object_member = reified_catch(&mut ir, identity);
        for body in [declaration, lambda, erased_object_member] {
            ir.functions.push(IrFunction {
                name: "eval".to_owned(),
                params: vec![],
                ret: Ty::Unit,
                body: Some(body),
                is_static: true,
                dispatch_receiver: None,
                param_checks: vec![],
            });
        }
        ir.signatures.insert(
            0,
            IrGenericSig {
                type_params: vec![IrTypeParameter {
                    name: "E".to_owned(),
                    semantic_name: identity.to_owned(),
                    bounds: vec![(Ty::obj("kotlin/Throwable"), false)],
                    variance: Default::default(),
                    reified: true,
                }],
                params: vec![],
                ret: None,
                supers: vec![],
            },
        );

        let lambdas = super::super::lambda_classes::LambdaMethods::for_test([1]);
        realize(&mut ir, &lambdas);

        assert!(ir.reified_catch_markers.contains_key(&declaration));
        assert!(ir.reified_catch_markers.contains_key(&lambda));
        assert!(!ir.reified_catch_markers.contains_key(&erased_object_member));
    }

    #[test]
    fn a_default_expression_realizes_operations_and_catches_only_for_the_reified_declaration() {
        let identity = "E@eval";
        let mut ir = IrFile::default();
        let body = ir.add_expr(IrExpr::UnitInstance);
        let operand = ir.add_expr(IrExpr::GetValue(0));
        let type_operation = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: operand,
            type_operand: Ty::ty_param(identity, Ty::obj("kotlin/Throwable")),
        });
        let class_literal = ir.add_expr(IrExpr::KClassLiteral {
            classifier: Some(Ty::ty_param(identity, Ty::obj("kotlin/Throwable"))),
            value: None,
            type_argument: true,
        });
        let declaration_default_catch = reified_catch(&mut ir, identity);
        let declaration_default = ir.add_expr(IrExpr::Block {
            stmts: vec![type_operation, class_literal],
            value: Some(declaration_default_catch),
        });
        let other_default = reified_catch(&mut ir, identity);
        ir.functions.push(IrFunction {
            name: "eval".to_owned(),
            params: vec![Ty::Unit],
            ret: Ty::Unit,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![],
        });
        ir.functions.push(IrFunction {
            name: "other".to_owned(),
            params: vec![Ty::Unit],
            ret: Ty::Unit,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![],
        });
        ir.signatures.insert(
            0,
            IrGenericSig {
                type_params: vec![IrTypeParameter {
                    name: "E".to_owned(),
                    semantic_name: identity.to_owned(),
                    bounds: vec![(Ty::obj("kotlin/Throwable"), false)],
                    variance: Default::default(),
                    reified: true,
                }],
                params: vec![],
                ret: None,
                supers: vec![],
            },
        );
        ir.fn_params.insert(
            0,
            crate::ir::FnParamInfo::source_defaults(
                vec!["block".to_owned()],
                vec![Some(declaration_default)],
            ),
        );
        ir.fn_params.insert(
            1,
            crate::ir::FnParamInfo::source_defaults(
                vec!["block".to_owned()],
                vec![Some(other_default)],
            ),
        );

        realize(&mut ir, &Default::default());

        assert!(matches!(
            ir.expr(type_operation),
            IrExpr::ReifiedTypeOp { cast: true, name, .. } if name == "E"
        ));
        assert!(matches!(
            ir.expr(class_literal),
            IrExpr::ReifiedClassMarker {
                name,
                kclass: true,
                ..
            } if name == "E"
        ));
        assert_eq!(
            ir.reified_catch_markers.get(&declaration_default_catch),
            Some(&vec![Some("E".to_owned())])
        );
        assert!(!ir.reified_catch_markers.contains_key(&other_default));
    }
}
