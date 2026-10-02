//! Call-site copies of anonymous objects that use a reified type parameter.
//!
//! An anonymous object's methods are separate functions. Substituting types only in the inlined
//! template leaves `new Declaration$1` pointing at the declaration class, whose `T` is erased.
//! This module clones that class when the expansion fixes a reified type its members use, and
//! points the copied construction at the clone. The declaration class stays in place so its own
//! body can keep the reified marker.

use std::collections::{HashMap, HashSet};

use crate::ir::{ClassId, ExprId, IrExpr};
use crate::types::{ty_subst_keep_unbound, Ty, TypeName};

use super::specialize_inline_copy;

/// The inline expansion an anonymous-class copy belongs to.
pub(super) struct CallSite<'a> {
    pub caller_declaration: crate::fir::DeclarationId,
    pub caller: Option<crate::ir::IrEnclosure>,
    pub caller_is_default: bool,
    pub caller_source_name: &'a str,
    pub inline_callee: crate::fir::CallableId,
    pub inline_callee_source_name: &'a str,
}

/// Point each copied construction of a reified anonymous object at a class specialized for this
/// expansion.
pub(super) fn specialize(
    ir: &mut crate::ir::IrFile,
    roots: impl IntoIterator<Item = ExprId>,
    bindings: &HashMap<String, Ty>,
    reified_bindings: &HashMap<String, Ty>,
    site: &CallSite<'_>,
) {
    if reified_bindings.is_empty() {
        return;
    }
    let expansion = Expansion {
        bindings,
        reified_bindings,
        site,
    };
    let mut seen = HashSet::new();
    for root in roots {
        retarget(ir, root, &expansion, &mut seen);
    }
}

struct Expansion<'a> {
    bindings: &'a HashMap<String, Ty>,
    reified_bindings: &'a HashMap<String, Ty>,
    site: &'a CallSite<'a>,
}

fn retarget(
    ir: &mut crate::ir::IrFile,
    root: ExprId,
    expansion: &Expansion<'_>,
    seen: &mut HashSet<ExprId>,
) {
    let mut pending = vec![root];
    let mut constructions = Vec::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if matches!(ir.expr(expression), IrExpr::New { .. }) {
            constructions.push(expression);
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    constructions.sort_unstable();
    for expression in constructions {
        let IrExpr::New { internal, .. } = ir.expr(expression).clone() else {
            continue;
        };
        let Some(specialized) = specialized_class(ir, internal, expression, expansion, seen) else {
            continue;
        };
        if let IrExpr::New { internal, .. } = &mut ir.exprs[expression as usize] {
            *internal = specialized;
        }
    }
}

fn specialized_class(
    ir: &mut crate::ir::IrFile,
    internal: TypeName,
    order: ExprId,
    expansion: &Expansion<'_>,
    seen: &mut HashSet<ExprId>,
) -> Option<TypeName> {
    let source = ir.class_id_by_name(internal)?;
    let anonymous = ir.classes.get(source as usize)?.is_anonymous_object;
    if !anonymous {
        return None;
    }
    if !class_uses_binding(ir, source, expansion.reified_bindings, &mut HashSet::new()) {
        return None;
    }
    let placeholder = crate::types::type_name(&format!("specialized-anon#{order}"));
    let source_methods = ir.classes[source as usize].methods.clone();
    let mut cloned_methods = Vec::with_capacity(source_methods.len());
    let mut owned = Vec::new();
    for method in source_methods.iter().copied() {
        let function = ir.functions.get(method as usize)?.clone();
        let body = function.body?;
        let (cloned_body, cloned) = crate::ir::clone_expression_dag(ir, body);
        let mut copied = cloned.values().copied().collect::<Vec<_>>();
        copied.sort_unstable();
        for copy in copied {
            specialize_inline_copy(ir, copy, expansion.bindings, expansion.reified_bindings)?;
            owned.push(copy);
        }
        let mut shape = function;
        shape.body = Some(cloned_body);
        shape.params = shape
            .params
            .iter()
            .copied()
            .map(|ty| ty_subst_keep_unbound(ty, expansion.bindings))
            .collect();
        shape.ret = ty_subst_keep_unbound(shape.ret, expansion.bindings);
        shape.dispatch_receiver = Some(placeholder);
        cloned_methods.push(crate::ir::clone_class_method(
            ir,
            method,
            shape,
            expansion.bindings,
        ));
    }
    let mut copy = ir.classes[source as usize].clone();
    copy.fq_name = placeholder;
    copy.is_source_declared = false;
    copy.enclosure = expansion.site.caller.or(copy.enclosure);
    copy.methods = cloned_methods.clone();
    specialize_class_types(&mut copy, expansion.bindings);
    let (init_body, init_owned) = clone_optional_body(ir, copy.init_body, expansion)?;
    copy.init_body = init_body;
    owned.extend(init_owned);
    let class_id = ir.add_class(copy);
    for method in &cloned_methods {
        ir.note_class_method(class_id, *method);
    }
    copy_capture_edges(ir, source, class_id, expansion.bindings);
    let source_name = ir.classes[source as usize].fq_name;
    let methods = source_methods
        .into_iter()
        .zip(cloned_methods)
        .collect::<Vec<_>>();
    copy_override_edges(ir, source_name, placeholder, &methods, expansion.bindings)?;
    remap_owned_class(ir, &owned, source, class_id, internal, placeholder);
    ir.specialized_anonymous_classes.insert(
        class_id,
        crate::ir::IrSpecializedAnonymousClass {
            source,
            order,
            caller_declaration: expansion.site.caller_declaration,
            caller: expansion.site.caller,
            caller_is_default: expansion.site.caller_is_default,
            caller_source_name: expansion.site.caller_source_name.to_string(),
            inline_callee: expansion.site.inline_callee,
            inline_callee_source_name: expansion.site.inline_callee_source_name.to_string(),
        },
    );
    for method in ir.classes[class_id as usize].methods.clone() {
        if let Some(body) = ir.functions[method as usize].body {
            retarget(ir, body, expansion, seen);
        }
    }
    if let Some(body) = ir.classes[class_id as usize].init_body {
        retarget(ir, body, expansion, seen);
    }
    Some(placeholder)
}

fn clone_optional_body(
    ir: &mut crate::ir::IrFile,
    body: Option<ExprId>,
    expansion: &Expansion<'_>,
) -> Option<(Option<ExprId>, Vec<ExprId>)> {
    let Some(body) = body else {
        return Some((None, Vec::new()));
    };
    let (cloned_body, cloned) = crate::ir::clone_expression_dag(ir, body);
    let mut copied = cloned.values().copied().collect::<Vec<_>>();
    copied.sort_unstable();
    for copy in &copied {
        specialize_inline_copy(ir, *copy, expansion.bindings, expansion.reified_bindings)?;
    }
    Some((Some(cloned_body), copied))
}

fn specialize_class_types(class: &mut crate::ir::IrClass, bindings: &HashMap<String, Ty>) {
    for ty in &mut class.supertypes {
        *ty = ty_subst_keep_unbound(*ty, bindings);
    }
    for field in &mut class.fields {
        field.ty = ty_subst_keep_unbound(field.ty, bindings);
    }
    for argument in &mut class.ctor_args {
        argument.ty = ty_subst_keep_unbound(argument.ty, bindings);
        if let Some(declared) = argument.declared_ty.as_mut() {
            *declared = ty_subst_keep_unbound(*declared, bindings);
        }
    }
    for property in &mut class.properties {
        property.ty = ty_subst_keep_unbound(property.ty, bindings);
        if let Some(storage) = property.storage_ty.as_mut() {
            *storage = ty_subst_keep_unbound(*storage, bindings);
        }
    }
    for argument in &mut class.super_ctor_params {
        *argument = ty_subst_keep_unbound(*argument, bindings);
    }
}

fn copy_capture_edges(
    ir: &mut crate::ir::IrFile,
    source: ClassId,
    target: ClassId,
    bindings: &HashMap<String, Ty>,
) {
    let fields = ir
        .shared_class_capture_fields
        .iter()
        .filter(|((class, _), _)| *class == source)
        .map(|((_, index), ty)| (*index, *ty))
        .collect::<Vec<_>>();
    for (index, ty) in fields {
        ir.shared_class_capture_fields
            .insert((target, index), ty_subst_keep_unbound(ty, bindings));
    }
    let parameters = ir
        .shared_super_capture_parameters
        .iter()
        .filter(|((class, _), _)| *class == source)
        .map(|((_, index), ty)| (*index, *ty))
        .collect::<Vec<_>>();
    for (index, ty) in parameters {
        ir.shared_super_capture_parameters
            .insert((target, index), ty_subst_keep_unbound(ty, bindings));
    }
    let identities = ir
        .class_capture_identities
        .iter()
        .filter(|((class, _), _)| *class == source)
        .map(|((_, index), identity)| (*index, *identity))
        .collect::<Vec<_>>();
    for (index, identity) in identities {
        ir.class_capture_identities
            .insert((target, index), identity);
    }
}

fn copy_override_edges(
    ir: &mut crate::ir::IrFile,
    source_name: TypeName,
    target_name: TypeName,
    methods: &[(crate::ir::FunId, crate::ir::FunId)],
    bindings: &HashMap<String, Ty>,
) -> Option<()> {
    let cloned = methods
        .iter()
        .copied()
        .collect::<HashMap<crate::ir::FunId, crate::ir::FunId>>();
    if let Some(edges) = ir.function_overrides.get(&source_name).cloned() {
        let mut copied = Vec::with_capacity(edges.len());
        for mut edge in edges {
            if edge.implementation_owner == source_name {
                edge.implementation_function = Some(cloned_override_function(
                    ir,
                    edge.implementation_function,
                    edge.implementation,
                    &cloned,
                )?);
                edge.implementation_owner = target_name;
                edge.applied_parameters = edge
                    .applied_parameters
                    .iter()
                    .copied()
                    .map(|ty| ty_subst_keep_unbound(ty, bindings))
                    .collect();
                edge.applied_result = ty_subst_keep_unbound(edge.applied_result, bindings);
                edge.implementation_parameters = edge
                    .implementation_parameters
                    .iter()
                    .copied()
                    .map(|ty| ty_subst_keep_unbound(ty, bindings))
                    .collect();
                edge.implementation_result =
                    ty_subst_keep_unbound(edge.implementation_result, bindings);
            }
            copied.push(edge);
        }
        ir.function_overrides.insert(target_name, copied);
    }
    if let Some(edges) = ir.property_overrides.get(&source_name).cloned() {
        let mut copied = Vec::with_capacity(edges.len());
        for mut edge in edges {
            if edge.implementation_owner == source_name {
                edge.implementation_getter =
                    retarget_optional_function(edge.implementation_getter, &cloned)?;
                edge.implementation_setter =
                    retarget_optional_function(edge.implementation_setter, &cloned)?;
                edge.implementation_owner = target_name;
                edge.applied_type = ty_subst_keep_unbound(edge.applied_type, bindings);
                edge.implementation_type =
                    ty_subst_keep_unbound(edge.implementation_type, bindings);
                edge.implementation_receiver = edge
                    .implementation_receiver
                    .map(|ty| ty_subst_keep_unbound(ty, bindings));
            }
            copied.push(edge);
        }
        ir.property_overrides.insert(target_name, copied);
    }
    Some(())
}

fn cloned_override_function(
    ir: &crate::ir::IrFile,
    recorded: Option<crate::ir::FunId>,
    implementation: crate::fir::ResolvedFunctionOverrideTarget,
    cloned: &HashMap<crate::ir::FunId, crate::ir::FunId>,
) -> Option<crate::ir::FunId> {
    let source = recorded.or_else(|| match implementation {
        crate::fir::ResolvedFunctionOverrideTarget::Module(declaration) => {
            ir.checked_callable_functions.get(&declaration).copied()
        }
        crate::fir::ResolvedFunctionOverrideTarget::External(_) => None,
    })?;
    cloned.get(&source).copied()
}

fn retarget_optional_function(
    function: Option<crate::ir::FunId>,
    cloned: &HashMap<crate::ir::FunId, crate::ir::FunId>,
) -> Option<Option<crate::ir::FunId>> {
    match function {
        Some(function) => cloned.get(&function).copied().map(Some),
        None => Some(None),
    }
}

fn remap_owned_class(
    ir: &mut crate::ir::IrFile,
    owned: &[ExprId],
    from: ClassId,
    to: ClassId,
    from_name: TypeName,
    to_name: TypeName,
) {
    for &expression in owned {
        let Some(node) = ir.exprs.get_mut(expression as usize) else {
            continue;
        };
        remap_class(node, from, to, from_name, to_name);
    }
}

fn remap_class(
    node: &mut IrExpr,
    from: ClassId,
    to: ClassId,
    from_name: TypeName,
    to_name: TypeName,
) {
    let class_id = |class: &mut ClassId| {
        if *class == from {
            *class = to;
        }
    };
    let name = |name: &mut TypeName| {
        if *name == from_name {
            *name = to_name;
        }
    };
    match node {
        IrExpr::GetField { class, .. }
        | IrExpr::LateinitInitialized { class, .. }
        | IrExpr::SetField { class, .. }
        | IrExpr::MethodCall { class, .. } => class_id(class),
        IrExpr::StaticInstance { owner, ty, .. } => {
            class_id(owner);
            class_id(ty);
        }
        IrExpr::New { internal, .. } => name(internal),
        IrExpr::EnclosingInstance { inner, outer, .. } => {
            name(inner);
            name(outer);
        }
        IrExpr::PropertyRead { owner, .. } | IrExpr::PropertyWrite { owner, .. } => name(owner),
        _ => {}
    }
}

fn class_uses_binding(
    ir: &crate::ir::IrFile,
    class: ClassId,
    bindings: &HashMap<String, Ty>,
    seen: &mut HashSet<ClassId>,
) -> bool {
    if bindings.is_empty() || !seen.insert(class) {
        return false;
    }
    let Some(class) = ir.classes.get(class as usize) else {
        return false;
    };
    let mut roots = class
        .methods
        .iter()
        .filter_map(|method| ir.functions.get(*method as usize)?.body)
        .collect::<Vec<_>>();
    roots.extend(class.init_body);
    for root in roots {
        if dag_uses_binding(ir, root, bindings, seen) {
            return true;
        }
    }
    false
}

fn dag_uses_binding(
    ir: &crate::ir::IrFile,
    root: ExprId,
    bindings: &HashMap<String, Ty>,
    classes: &mut HashSet<ClassId>,
) -> bool {
    let mut pending = vec![root];
    let mut seen = HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        if node_uses_binding(ir.expr(expression), bindings) {
            return true;
        }
        if let IrExpr::New { internal, .. } = ir.expr(expression) {
            if let Some(class) = ir.class_id_by_name(*internal) {
                if class_uses_binding(ir, class, bindings, classes) {
                    return true;
                }
            }
        }
    }
    false
}

fn node_uses_binding(expression: &IrExpr, bindings: &HashMap<String, Ty>) -> bool {
    let uses = |ty: Ty| ty_subst_keep_unbound(ty, bindings) != ty;
    match expression {
        IrExpr::TypeOp { type_operand, .. } => uses(*type_operand),
        IrExpr::KClassLiteral { classifier, .. } => classifier.is_some_and(uses),
        IrExpr::Call {
            callee: crate::ir::Callee::Intrinsic { operation, .. },
            ..
        } => match operation {
            crate::ir::IrIntrinsic::TypeOf { ty } => uses(*ty),
            _ => false,
        },
        _ => false,
    }
}
