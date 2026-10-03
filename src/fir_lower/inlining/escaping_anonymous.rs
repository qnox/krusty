//! Call-site copies of anonymous objects that use a reified type parameter.
//!
//! An anonymous object's methods are separate functions. Substituting types only in the inlined
//! template leaves `new Declaration$1` pointing at the declaration class, whose `T` is erased.
//! This module clones that class when the expansion fixes a reified type its members use, and
//! points the copied construction at the clone. The declaration class stays in place so its own
//! body can keep the reified marker.

use std::collections::{HashMap, HashSet};

use crate::ir::{ClassId, ExprId, FunId, IrExpr};
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
/// expansion. A class that does not use the reified binding is left alone. Once it does, a missing
/// body or accessor mapping fails the lowering instead of keeping the declaration class.
pub(super) fn specialize(
    ir: &mut crate::ir::IrFile,
    roots: impl IntoIterator<Item = ExprId>,
    bindings: &HashMap<String, Ty>,
    reified_bindings: &HashMap<String, Ty>,
    site: &CallSite<'_>,
) -> Result<(), super::super::FirLoweringFailure> {
    if reified_bindings.is_empty() {
        return Ok(());
    }
    let expansion = Expansion {
        bindings,
        reified_bindings,
        site,
    };
    let mut seen = HashSet::new();
    for root in roots {
        retarget(ir, root, &expansion, &mut seen)?;
    }
    Ok(())
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
) -> Result<(), super::super::FirLoweringFailure> {
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
        let Some(specialized) = specialized_class(ir, internal, expression, expansion, seen)?
        else {
            continue;
        };
        if let IrExpr::New { internal, .. } = &mut ir.exprs[expression as usize] {
            *internal = specialized;
        }
    }
    Ok(())
}

fn specialized_class(
    ir: &mut crate::ir::IrFile,
    internal: TypeName,
    order: ExprId,
    expansion: &Expansion<'_>,
    seen: &mut HashSet<ExprId>,
) -> Result<Option<TypeName>, super::super::FirLoweringFailure> {
    let Some(source) = ir.class_id_by_name(internal) else {
        return Ok(None);
    };
    let anonymous = ir
        .classes
        .get(source as usize)
        .is_some_and(|class| class.is_anonymous_object);
    if !anonymous {
        return Ok(None);
    }
    if !class_uses_binding(ir, source, expansion.reified_bindings, &mut HashSet::new()) {
        return Ok(None);
    }
    let class_name = ir.classes[source as usize].fq_name;
    let field_count = u32::try_from(ir.classes[source as usize].fields.len())
        .map_err(|_| malformed(class_name))?;
    let property_count = u32::try_from(ir.classes[source as usize].properties.len())
        .map_err(|_| malformed(class_name))?;
    ir.reified_anonymous_declarations.insert(source);
    let placeholder = crate::types::type_name(&format!("specialized-anon#{order}"));
    let source_methods = ir.classes[source as usize].methods.clone();
    let mut cloned_methods = Vec::with_capacity(source_methods.len());
    let mut owned = Vec::new();
    for method in source_methods.iter().copied() {
        let function = ir
            .functions
            .get(method as usize)
            .cloned()
            .ok_or_else(|| malformed(class_name))?;
        let body = function.body.ok_or_else(|| malformed(class_name))?;
        let (cloned_body, cloned) = crate::ir::clone_expression_dag(ir, body);
        let mut copied = cloned.values().copied().collect::<Vec<_>>();
        copied.sort_unstable();
        for copy in copied {
            specialize_inline_copy(ir, copy, expansion.bindings, expansion.reified_bindings)
                .ok_or_else(|| malformed(class_name))?;
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
    let cloned = source_methods
        .iter()
        .copied()
        .zip(cloned_methods.iter().copied())
        .collect::<HashMap<_, _>>();
    let mut copy = ir.classes[source as usize].clone();
    copy.fq_name = placeholder;
    copy.is_source_declared = false;
    copy.enclosure = expansion.site.caller.or(copy.enclosure);
    copy.methods = cloned_methods.clone();
    specialize_class_types(&mut copy, expansion.bindings);
    remap_property_accessors(&mut copy, &cloned).map_err(|()| malformed(class_name))?;
    let (init_body, init_owned) = clone_optional_body(ir, copy.init_body, expansion, class_name)?;
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
            method_clones: cloned,
            bindings: expansion.bindings.clone(),
            reified_bindings: expansion.reified_bindings.clone(),
            field_count,
            property_count,
        },
    );
    for method in ir.classes[class_id as usize].methods.clone() {
        if let Some(body) = ir.functions[method as usize].body {
            retarget(ir, body, expansion, seen)?;
        }
    }
    if let Some(body) = ir.classes[class_id as usize].init_body {
        retarget(ir, body, expansion, seen)?;
    }
    Ok(Some(placeholder))
}

fn clone_optional_body(
    ir: &mut crate::ir::IrFile,
    body: Option<ExprId>,
    expansion: &Expansion<'_>,
    class_name: TypeName,
) -> Result<(Option<ExprId>, Vec<ExprId>), super::super::FirLoweringFailure> {
    let Some(body) = body else {
        return Ok((None, Vec::new()));
    };
    let (cloned_body, cloned) = crate::ir::clone_expression_dag(ir, body);
    let mut copied = cloned.values().copied().collect::<Vec<_>>();
    copied.sort_unstable();
    for copy in &copied {
        specialize_inline_copy(ir, *copy, expansion.bindings, expansion.reified_bindings)
            .ok_or_else(|| malformed(class_name))?;
    }
    Ok((Some(cloned_body), copied))
}

fn remap_property_accessors(
    class: &mut crate::ir::IrClass,
    cloned: &HashMap<FunId, FunId>,
) -> Result<(), ()> {
    for property in &mut class.properties {
        property.getter = mapped_function(property.getter, cloned)?;
        property.setter = mapped_function(property.setter, cloned)?;
    }
    Ok(())
}

fn mapped_function(
    function: Option<FunId>,
    cloned: &HashMap<FunId, FunId>,
) -> Result<Option<FunId>, ()> {
    match function {
        Some(function) => cloned.get(&function).copied().map(Some).ok_or(()),
        None => Ok(None),
    }
}

fn malformed(class: TypeName) -> super::super::FirLoweringFailure {
    super::super::FirLoweringFailure::MalformedReifiedAnonymousObject { class }
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
    methods: &[(FunId, FunId)],
    bindings: &HashMap<String, Ty>,
) -> Result<(), super::super::FirLoweringFailure> {
    let cloned = methods.iter().copied().collect::<HashMap<FunId, FunId>>();
    if let Some(edges) = ir.function_overrides.get(&source_name).cloned() {
        let mut copied = Vec::with_capacity(edges.len());
        for mut edge in edges {
            if edge.implementation_owner == source_name {
                edge.implementation_function = Some(
                    cloned_override_function(
                        ir,
                        edge.implementation_function,
                        edge.implementation,
                        &cloned,
                    )
                    .ok_or_else(|| malformed(source_name))?,
                );
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
    Ok(())
}

/// Property accessors and member extensions are materialized after the class copy. Publishing
/// them here uses the completed function clone map; doing it during the copy would read layouts
/// that do not exist yet and would leave the copy without those functions.
fn publish_property_members(
    ir: &mut crate::ir::IrFile,
    source: ClassId,
    source_name: TypeName,
    target_name: TypeName,
    cloned: &HashMap<FunId, FunId>,
    bindings: &HashMap<String, Ty>,
) -> Result<(), super::super::FirLoweringFailure> {
    if let Some(edges) = ir.property_overrides.get(&source_name).cloned() {
        let mut copied = Vec::with_capacity(edges.len());
        for mut edge in edges {
            if edge.implementation_owner == source_name {
                let (getter, setter) =
                    copied_property_accessors(ir, &edge, source, source_name, cloned)?;
                edge.implementation_getter = getter;
                edge.implementation_setter = setter;
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
    copy_member_extensions(ir, source_name, target_name, cloned, bindings)?;
    Ok(())
}

/// Finish call-site copies after property layouts exist.
///
/// The copy is taken while an inline call is lowered, and accessor functions are published only
/// once every body has been consumed. This clones those functions onto the copy, appends the
/// backing fields materialized with them, and fills property-override accessors from the clone map.
pub(super) fn publish_accessors(
    ir: &mut crate::ir::IrFile,
) -> Result<(), super::super::FirLoweringFailure> {
    let mut done = HashSet::<ClassId>::new();
    loop {
        let mut pending = ir
            .specialized_anonymous_classes
            .keys()
            .copied()
            .filter(|class| !done.contains(class))
            .collect::<Vec<_>>();
        if pending.is_empty() {
            break;
        }
        pending.sort_unstable();
        for copy_id in pending {
            done.insert(copy_id);
            publish_one_copy(ir, copy_id)?;
        }
    }
    Ok(())
}

fn publish_one_copy(
    ir: &mut crate::ir::IrFile,
    copy_id: ClassId,
) -> Result<(), super::super::FirLoweringFailure> {
    let spec = ir
        .specialized_anonymous_classes
        .get(&copy_id)
        .cloned()
        .ok_or_else(|| malformed(crate::types::type_name("specialized-anonymous")))?;
    let source = spec.source;
    let source_name = ir
        .classes
        .get(source as usize)
        .map(|class| class.fq_name)
        .ok_or_else(|| malformed(crate::types::type_name("specialized-anonymous")))?;
    let copy_name = ir
        .classes
        .get(copy_id as usize)
        .map(|class| class.fq_name)
        .ok_or_else(|| malformed(source_name))?;
    if ir.classes[copy_id as usize].fields.len() != spec.field_count as usize
        || ir.classes[copy_id as usize].properties.len() != spec.property_count as usize
    {
        return Err(malformed(source_name));
    }
    let mut clones = spec.method_clones.clone();
    let mut needed = ir.classes[source as usize].methods.clone();
    for property in &ir.classes[source as usize].properties {
        needed.extend(property.getter);
        needed.extend(property.setter);
    }
    if let Some(extensions) = ir.member_ext_props.get(&source_name) {
        for property in extensions {
            needed.push(property.getter);
            needed.extend(property.setter);
        }
    }
    needed.retain(|function| !clones.contains_key(function));
    needed.sort_unstable();
    needed.dedup();
    let mut nested = Vec::new();
    for method in needed {
        let (cloned, owned) = clone_member_function(
            ir,
            method,
            copy_name,
            &spec.bindings,
            &spec.reified_bindings,
            source_name,
        )?;
        remap_owned_class(ir, &owned, source, copy_id, source_name, copy_name);
        clones.insert(method, cloned);
        ir.classes[copy_id as usize].methods.push(cloned);
        ir.note_class_method(copy_id, cloned);
        if let Some(body) = ir.functions[cloned as usize].body {
            nested.push(body);
        }
    }
    let new_fields = ir.classes[source as usize].fields[spec.field_count as usize..].to_vec();
    for mut field in new_fields {
        field.ty = ty_subst_keep_unbound(field.ty, &spec.bindings);
        ir.classes[copy_id as usize].fields.push(field);
    }
    let source_properties = ir.classes[source as usize].properties.clone();
    if source_properties.len() < spec.property_count as usize {
        return Err(malformed(source_name));
    }
    for (index, source_property) in source_properties.iter().enumerate() {
        let getter = mapped_function(source_property.getter, &clones)
            .map_err(|()| malformed(source_name))?;
        let setter = mapped_function(source_property.setter, &clones)
            .map_err(|()| malformed(source_name))?;
        if index < spec.property_count as usize {
            let property = &mut ir.classes[copy_id as usize].properties[index];
            property.getter = getter;
            property.setter = setter;
            continue;
        }
        let mut property = source_property.clone();
        property.ty = ty_subst_keep_unbound(property.ty, &spec.bindings);
        if let Some(storage) = property.storage_ty.as_mut() {
            *storage = ty_subst_keep_unbound(*storage, &spec.bindings);
        }
        for (_, _, ty) in &mut property.context_params {
            *ty = ty_subst_keep_unbound(*ty, &spec.bindings);
        }
        property.getter = getter;
        property.setter = setter;
        ir.classes[copy_id as usize].properties.push(property);
    }
    let init_clones = clone_initializer(ir, source, copy_id, &spec, source_name, copy_name)?;
    if init_clones.is_none()
        && source_properties
            .iter()
            .skip(spec.property_count as usize)
            .any(|property| property.initializer.is_some())
    {
        return Err(malformed(source_name));
    }
    if let Some(init_clones) = &init_clones {
        let initializers = ir.classes[source as usize]
            .properties
            .iter()
            .map(|property| property.initializer)
            .collect::<Vec<_>>();
        for (index, initializer) in initializers.into_iter().enumerate() {
            let Some(initializer) = initializer else {
                continue;
            };
            let cloned = init_clones
                .get(&initializer)
                .copied()
                .ok_or_else(|| malformed(source_name))?;
            ir.classes[copy_id as usize].properties[index].initializer = Some(cloned);
        }
        if let Some(body) = ir.classes[copy_id as usize].init_body {
            nested.push(body);
        }
    }
    publish_property_members(ir, source, source_name, copy_name, &clones, &spec.bindings)?;
    specialize(
        ir,
        nested,
        &spec.bindings,
        &spec.reified_bindings,
        &CallSite {
            caller_declaration: spec.caller_declaration,
            caller: spec.caller,
            caller_is_default: spec.caller_is_default,
            caller_source_name: &spec.caller_source_name,
            inline_callee: spec.inline_callee,
            inline_callee_source_name: &spec.inline_callee_source_name,
        },
    )?;
    if let Some(record) = ir.specialized_anonymous_classes.get_mut(&copy_id) {
        record.method_clones = clones;
    }
    Ok(())
}

fn clone_member_function(
    ir: &mut crate::ir::IrFile,
    method: FunId,
    dispatch_receiver: TypeName,
    bindings: &HashMap<String, Ty>,
    reified_bindings: &HashMap<String, Ty>,
    class_name: TypeName,
) -> Result<(FunId, Vec<ExprId>), super::super::FirLoweringFailure> {
    let function = ir
        .functions
        .get(method as usize)
        .cloned()
        .ok_or_else(|| malformed(class_name))?;
    let body = function.body.ok_or_else(|| malformed(class_name))?;
    let (cloned_body, cloned) = crate::ir::clone_expression_dag(ir, body);
    let mut copied = cloned.values().copied().collect::<Vec<_>>();
    copied.sort_unstable();
    for copy in &copied {
        specialize_inline_copy(ir, *copy, bindings, reified_bindings)
            .ok_or_else(|| malformed(class_name))?;
    }
    let mut shape = function;
    shape.body = Some(cloned_body);
    shape.params = shape
        .params
        .iter()
        .copied()
        .map(|ty| ty_subst_keep_unbound(ty, bindings))
        .collect();
    shape.ret = ty_subst_keep_unbound(shape.ret, bindings);
    shape.dispatch_receiver = Some(dispatch_receiver);
    Ok((
        crate::ir::clone_class_method(ir, method, shape, bindings),
        copied,
    ))
}

fn clone_initializer(
    ir: &mut crate::ir::IrFile,
    source: ClassId,
    copy_id: ClassId,
    spec: &crate::ir::IrSpecializedAnonymousClass,
    source_name: TypeName,
    copy_name: TypeName,
) -> Result<Option<HashMap<ExprId, ExprId>>, super::super::FirLoweringFailure> {
    let Some(body) = ir.classes[source as usize].init_body else {
        return Ok(None);
    };
    let (cloned_body, cloned) = crate::ir::clone_expression_dag(ir, body);
    let mut copied = cloned.values().copied().collect::<Vec<_>>();
    copied.sort_unstable();
    for copy in &copied {
        specialize_inline_copy(ir, *copy, &spec.bindings, &spec.reified_bindings)
            .ok_or_else(|| malformed(source_name))?;
    }
    remap_owned_class(ir, &copied, source, copy_id, source_name, copy_name);
    ir.classes[copy_id as usize].init_body = Some(cloned_body);
    Ok(Some(cloned))
}

fn copied_property_accessors(
    ir: &crate::ir::IrFile,
    edge: &crate::ir::IrPropertyOverride,
    source: ClassId,
    source_name: TypeName,
    cloned: &HashMap<FunId, FunId>,
) -> Result<(Option<FunId>, Option<FunId>), super::super::FirLoweringFailure> {
    let (layout_getter, layout_setter) = layout_accessors(ir, edge, source, source_name, cloned)?;
    let getter = match edge.implementation_getter {
        Some(function) => Some(require_clone(function, cloned, source_name)?),
        None => layout_getter,
    };
    let setter = match edge.implementation_setter {
        Some(function) => Some(require_clone(function, cloned, source_name)?),
        None => layout_setter,
    };
    Ok((getter, setter))
}

fn layout_accessors(
    ir: &crate::ir::IrFile,
    edge: &crate::ir::IrPropertyOverride,
    source: ClassId,
    source_name: TypeName,
    cloned: &HashMap<FunId, FunId>,
) -> Result<(Option<FunId>, Option<FunId>), super::super::FirLoweringFailure> {
    let crate::fir::ResolvedPropertyOverrideTarget::Module(property) = edge.implementation else {
        return Err(malformed(source_name));
    };
    let Some(layout) = ir.local_property_layouts.get(&property) else {
        return Err(malformed(source_name));
    };
    match layout {
        crate::ir::IrLocalPropertyLayout::Member {
            class,
            owner,
            getter,
            setter,
            ..
        } if *class == source || *owner == source_name => Ok((
            mapped_function(*getter, cloned).map_err(|()| malformed(source_name))?,
            mapped_function(*setter, cloned).map_err(|()| malformed(source_name))?,
        )),
        crate::ir::IrLocalPropertyLayout::MemberExtension {
            owner,
            getter,
            setter,
            ..
        } if *owner == source_name => Ok((
            Some(require_clone(*getter, cloned, source_name)?),
            mapped_function(*setter, cloned).map_err(|()| malformed(source_name))?,
        )),
        _ => Err(malformed(source_name)),
    }
}

fn copy_member_extensions(
    ir: &mut crate::ir::IrFile,
    source_name: TypeName,
    target_name: TypeName,
    cloned: &HashMap<FunId, FunId>,
    bindings: &HashMap<String, Ty>,
) -> Result<(), super::super::FirLoweringFailure> {
    let Some(properties) = ir.member_ext_props.get(&source_name).cloned() else {
        return Ok(());
    };
    let mut copied = Vec::with_capacity(properties.len());
    for mut property in properties {
        property.getter = require_clone(property.getter, cloned, source_name)?;
        property.setter =
            mapped_function(property.setter, cloned).map_err(|()| malformed(source_name))?;
        property.receiver = ty_subst_keep_unbound(property.receiver, bindings);
        property.ty = ty_subst_keep_unbound(property.ty, bindings);
        copied.push(property);
    }
    ir.member_ext_props.insert(target_name, copied);
    Ok(())
}

fn require_clone(
    function: FunId,
    cloned: &HashMap<FunId, FunId>,
    class: TypeName,
) -> Result<FunId, super::super::FirLoweringFailure> {
    cloned
        .get(&function)
        .copied()
        .ok_or_else(|| malformed(class))
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
    let Some(class_decl) = ir.classes.get(class as usize) else {
        return false;
    };
    let mut functions = class_decl.methods.clone();
    functions.extend(
        class_decl
            .properties
            .iter()
            .flat_map(|property| property.getter.into_iter().chain(property.setter)),
    );
    if let Some(extensions) = ir.member_ext_props.get(&class_decl.fq_name) {
        functions.extend(
            extensions
                .iter()
                .flat_map(|property| std::iter::once(property.getter).chain(property.setter)),
        );
    }
    let mut roots = functions
        .into_iter()
        .filter_map(|function| ir.functions.get(function as usize)?.body)
        .collect::<Vec<_>>();
    roots.extend(class_decl.init_body);
    // Accessor functions are materialized after inlining. Their checked bodies are already
    // expressions on the property, and a reified operation there must still select the copy.
    roots.extend(
        ir.checked_properties
            .values()
            .filter(|property| property.class == Some(class))
            .flat_map(|property| {
                property
                    .getter
                    .into_iter()
                    .chain(property.setter)
                    .chain(property.initializer)
            }),
    );
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
