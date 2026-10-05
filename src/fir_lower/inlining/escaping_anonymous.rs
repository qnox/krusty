//! Call-site copies of anonymous objects that use a reified type parameter, and of anonymous
//! objects that capture an inline lambda.
//!
//! An anonymous object's methods are separate functions. Substituting types only in the inlined
//! template leaves `new Declaration$1` pointing at the declaration class, whose `T` is erased.
//! This module clones that class when the expansion fixes a reified type its members use, and
//! points the copied construction at the clone. The declaration class stays in place so its own
//! body can keep the reified marker.
//!
//! A `crossinline` lambda captured by the object is the other reason to copy. kotlinc regenerates
//! that class at every call, and `typeOf` of the instance names the copy. A capture-free lambda is
//! inlined into the copy's methods and dropped from its constructor. A lambda that itself captures
//! values stays a constructor argument of the copy.

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

/// Point each copied construction of an anonymous object at a class specialized for this
/// expansion. A class that does not use the reified binding and does not capture an inline lambda
/// is left alone. Once it is copied, a missing body or accessor mapping fails the lowering instead
/// of keeping the declaration class.
pub(super) fn specialize(
    ir: &mut crate::ir::IrFile,
    roots: impl IntoIterator<Item = ExprId>,
    bindings: &HashMap<String, Ty>,
    reified_bindings: &HashMap<String, Ty>,
    site: &CallSite<'_>,
) -> Result<(), super::super::FirLoweringFailure> {
    let expansion = Expansion {
        bindings,
        reified_bindings,
        site,
    };
    let mut seen = HashSet::new();
    let mut renames = HashMap::new();
    let mut detached_impls = HashSet::new();
    let roots = roots.into_iter().collect::<Vec<_>>();
    for root in &roots {
        retarget(
            ir,
            *root,
            &expansion,
            &mut seen,
            &mut renames,
            &mut detached_impls,
        )?;
    }
    for root in &roots {
        remap_reachable_implementations(ir, &renames, *root, &mut detached_impls, &expansion);
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
    renames: &mut HashMap<TypeName, TypeName>,
    detached_impls: &mut HashSet<FunId>,
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
        let Some(specialized) = specialized_class(
            ir,
            internal,
            expression,
            expansion,
            seen,
            renames,
            detached_impls,
        )?
        else {
            continue;
        };
        // Point the construction at the call-site class now. Its capture prefix is still being
        // streamed independently from the class declaration, so constructor arity and the
        // declaration-erased descriptor are finalized by `publish_one_copy` after every body has
        // contributed its captures.
        if let IrExpr::New { internal: name, .. } = &mut ir.exprs[expression as usize] {
            *name = specialized;
        }
    }
    remap_reachable_implementations(ir, renames, root, detached_impls, expansion);
    note_caller_property_uses(ir, root);
    Ok(())
}

/// A shared lambda implementation detached for this anonymous-class copy is a call-site
/// specialization too. Recording that ownership keeps a suspend or class-materialized lambda on
/// the backend's specialized naming path instead of publishing a second source class identity.
fn remap_reachable_implementations(
    ir: &mut crate::ir::IrFile,
    renames: &HashMap<TypeName, TypeName>,
    root: ExprId,
    detached_impls: &mut HashSet<FunId>,
    expansion: &Expansion<'_>,
) {
    for detached in ir.remap_reachable_classifier_identities(renames, [root], detached_impls) {
        ir.specialized_functions.insert(
            detached.target,
            crate::ir::IrSpecializedFunction {
                source: detached.source,
                caller_declaration: expansion.site.caller_declaration,
                caller: expansion.site.caller,
                caller_is_default: expansion.site.caller_is_default,
                caller_source_name: expansion.site.caller_source_name.to_string(),
                inline_callee: expansion.site.inline_callee,
                inline_callee_source_name: expansion.site.inline_callee_source_name.to_string(),
                parent: detached.parent,
            },
        );
        if detached.parent.is_none() {
            ir.specialized_expansion_order
                .entry(detached.target)
                .or_insert(detached.order);
        }
    }
}

fn note_caller_property_uses(ir: &mut crate::ir::IrFile, root: ExprId) {
    let mut pending = vec![root];
    let mut seen = HashSet::new();
    let mut uses = Vec::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        let receiver = match ir.expr(expression) {
            IrExpr::Checked(
                crate::ir::IrCheckedOperation::PropertyRead {
                    dispatch_receiver, ..
                }
                | crate::ir::IrCheckedOperation::PropertyWrite {
                    dispatch_receiver, ..
                },
            ) => *dispatch_receiver,
            _ => None,
        };
        let Some(receiver) = receiver else {
            continue;
        };
        let IrExpr::New { internal, .. } = ir.expr(receiver) else {
            continue;
        };
        let Some(class) = ir.class_id_by_name(*internal) else {
            continue;
        };
        if ir.specialized_anonymous_classes.contains_key(&class) {
            uses.push((class, expression));
        }
    }
    for (class, expression) in uses {
        let Some(record) = ir.specialized_anonymous_classes.get_mut(&class) else {
            continue;
        };
        if !record.caller_property_uses.contains(&expression) {
            record.caller_property_uses.push(expression);
        }
    }
}

fn retarget_caller_property_uses(
    ir: &mut crate::ir::IrFile,
    copy_id: ClassId,
    spec: &crate::ir::IrSpecializedAnonymousClass,
    source_name: TypeName,
) -> Result<(), super::super::FirLoweringFailure> {
    for expression in spec.caller_property_uses.clone() {
        retarget_caller_property_use(ir, expression, copy_id, source_name)?;
    }
    Ok(())
}

fn retarget_caller_property_use(
    ir: &mut crate::ir::IrFile,
    expression: ExprId,
    copy_id: ClassId,
    source_name: TypeName,
) -> Result<(), super::super::FirLoweringFailure> {
    let IrExpr::Checked(operation) = ir.expr(expression).clone() else {
        return Ok(());
    };
    let (target, receiver, extension_receiver, context_arguments, value) = match operation {
        crate::ir::IrCheckedOperation::PropertyRead {
            target,
            dispatch_receiver,
            extension_receiver,
            context_arguments,
            ..
        } => (
            target,
            dispatch_receiver,
            extension_receiver,
            context_arguments,
            None,
        ),
        crate::ir::IrCheckedOperation::PropertyWrite {
            target,
            dispatch_receiver,
            extension_receiver,
            context_arguments,
            value,
            ..
        } => (
            target,
            dispatch_receiver,
            extension_receiver,
            context_arguments,
            Some(value),
        ),
        _ => return Ok(()),
    };
    if extension_receiver.is_some() || !context_arguments.is_empty() {
        return Err(malformed(source_name));
    }
    let name = match ir.local_property_layouts.get(&target) {
        Some(crate::ir::IrLocalPropertyLayout::Member { name, .. }) => name.clone(),
        _ => return Err(malformed(source_name)),
    };
    let property = ir.classes[copy_id as usize]
        .properties
        .iter()
        .find(|property| property.name == name)
        .ok_or_else(|| malformed(source_name))?;
    let ty = property.ty;
    let owner = ir.classes[copy_id as usize].fq_name;
    ir.exprs[expression as usize] = match value {
        Some(value) => IrExpr::PropertyWrite {
            receiver,
            owner,
            name,
            value,
            ty,
            interface: false,
            operation: Some(expression),
        },
        None => IrExpr::PropertyRead {
            receiver,
            owner,
            name,
            ty,
            interface: false,
            operation: Some(expression),
        },
    };
    Ok(())
}

fn specialized_class(
    ir: &mut crate::ir::IrFile,
    internal: TypeName,
    order: ExprId,
    expansion: &Expansion<'_>,
    seen: &mut HashSet<ExprId>,
    renames: &mut HashMap<TypeName, TypeName>,
    detached_impls: &mut HashSet<FunId>,
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
    let uses_reified =
        class_uses_binding(ir, source, expansion.reified_bindings, &mut HashSet::new());
    let lambdas = inline_lambda_arguments(ir, source, order);
    if !uses_reified && lambdas.is_empty() {
        return Ok(None);
    }
    let class_name = ir.classes[source as usize].fq_name;
    if uses_reified {
        ir.reified_anonymous_declarations.insert(source);
    }
    let field_count = u32::try_from(ir.classes[source as usize].fields.len())
        .map_err(|_| malformed(class_name))?;
    let property_count = u32::try_from(ir.classes[source as usize].properties.len())
        .map_err(|_| malformed(class_name))?;
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
        // Member descriptors stay the declaration's erasure. Reified operations in the body are
        // specialized above; substituting the signature would emit a concrete descriptor plus a
        // bridge where kotlinc keeps the erased member.
        shape.dispatch_receiver = Some(placeholder);
        cloned_methods.push(crate::ir::clone_class_method(
            ir,
            method,
            shape,
            &HashMap::new(),
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
    remap_property_accessors(&mut copy, &cloned).map_err(|()| malformed(class_name))?;
    let source_init = ir.classes[source as usize].init_body;
    let covered = source_init
        .map(|body| expression_ids(ir, body))
        .unwrap_or_default();
    let property_roots = property_roots(ir, source)
        .into_iter()
        .filter(|root| !covered.contains(root))
        .collect::<Vec<_>>();
    let (init_body, init_owned) = clone_optional_body(ir, copy.init_body, expansion, class_name)?;
    copy.init_body = init_body;
    owned.extend(init_owned);
    // Property initializers and accessor bodies are still checked expressions here. They stay on
    // the specialization record, not in the constructor: accessor functions are assembled only
    // after every inline expansion. A later expansion has to see the specialized reified type on
    // this copy, or it will reuse the copy and leave the outer parameter unsubstituted.
    let mut property_copies = Vec::new();
    for root in property_roots {
        let (cloned_root, cloned_owned) =
            clone_optional_body(ir, Some(root), expansion, class_name)?;
        let Some(cloned_root) = cloned_root else {
            continue;
        };
        property_copies.push(cloned_root);
        owned.extend(cloned_owned);
    }
    let class_id = ir.add_class(copy);
    for method in &cloned_methods {
        ir.note_class_method(class_id, *method);
    }
    copy_capture_edges(ir, source, class_id);
    let source_name = ir.classes[source as usize].fq_name;
    let methods = source_methods
        .into_iter()
        .zip(cloned_methods)
        .collect::<Vec<_>>();
    copy_override_edges(ir, source_name, placeholder, &methods)?;
    remap_owned_class(ir, &owned, source, class_id, internal, placeholder);
    let omitted_capture_fields = consume_trivially_spliceable_lambdas(
        ir,
        class_id,
        order,
        &property_copies,
        &lambdas,
        class_name,
    )?;
    renames.insert(internal, placeholder);
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
            omitted_capture_fields,
            caller_property_uses: Vec::new(),
            pending_property_roots: property_copies,
        },
    );
    for method in ir.classes[class_id as usize].methods.clone() {
        if let Some(body) = ir.functions[method as usize].body {
            retarget(ir, body, expansion, seen, renames, detached_impls)?;
        }
    }
    if let Some(body) = ir.classes[class_id as usize].init_body {
        retarget(ir, body, expansion, seen, renames, detached_impls)?;
    }
    let pending_roots = ir
        .specialized_anonymous_classes
        .get(&class_id)
        .map(|record| record.pending_property_roots.clone())
        .unwrap_or_default();
    for root in pending_roots {
        retarget(ir, root, expansion, seen, renames, detached_impls)?;
    }
    Ok(Some(placeholder))
}

fn expression_ids(ir: &crate::ir::IrFile, root: ExprId) -> HashSet<ExprId> {
    let mut pending = vec![root];
    let mut seen = HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    seen
}

fn property_roots(ir: &crate::ir::IrFile, class: ClassId) -> Vec<ExprId> {
    let mut roots = Vec::new();
    for property in ir.checked_properties.values() {
        if property.class != Some(class) {
            continue;
        }
        roots.extend(property.getter);
        roots.extend(property.setter);
        roots.extend(property.initializer);
    }
    // A copy taken before accessors exist keeps those roots on its specialization record.
    // The next expansion clones that record; the copy has no checked-property entry of its own.
    if let Some(record) = ir.specialized_anonymous_classes.get(&class) {
        roots.extend(record.pending_property_roots.iter().copied());
    }
    roots.sort_unstable();
    roots.dedup();
    roots
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

fn copy_capture_edges(ir: &mut crate::ir::IrFile, source: ClassId, target: ClassId) {
    let fields = ir
        .shared_class_capture_fields
        .iter()
        .filter(|((class, _), _)| *class == source)
        .map(|((_, index), ty)| (*index, *ty))
        .collect::<Vec<_>>();
    for (index, ty) in fields {
        ir.shared_class_capture_fields.insert((target, index), ty);
    }
    let parameters = ir
        .shared_super_capture_parameters
        .iter()
        .filter(|((class, _), _)| *class == source)
        .map(|((_, index), ty)| (*index, *ty))
        .collect::<Vec<_>>();
    for (index, ty) in parameters {
        ir.shared_super_capture_parameters
            .insert((target, index), ty);
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
            }
            copied.push(edge);
        }
        ir.function_overrides.insert(target_name, copied);
    }
    // The copy inherits the same interface defaults as the class it copies.
    if let Some(defaults) = ir.inherited_defaults.get(&source_name).cloned() {
        ir.inherited_defaults.insert(target_name, defaults);
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
            }
            copied.push(edge);
        }
        ir.property_overrides.insert(target_name, copied);
    }
    copy_member_extensions(ir, source_name, target_name, cloned)?;
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
    if ir.classes[copy_id as usize].fields.len() + spec.omitted_capture_fields as usize
        != spec.field_count as usize
        || ir.classes[copy_id as usize].properties.len() != spec.property_count as usize
    {
        return Err(malformed(source_name));
    }
    // A field appended to the declaration after the copy is indexed from `field_count`. Dropping a
    // capture shifts every later index, so that append is not the copy's next field.
    if spec.omitted_capture_fields != 0
        && ir.classes[source as usize].fields.len() > spec.field_count as usize
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
    ir.classes[copy_id as usize].fields.extend(new_fields);
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
    publish_property_members(ir, source, source_name, copy_name, &clones)?;
    retarget_copied_property_calls(ir, copy_id, source, source_name, &clones)?;
    retarget_caller_property_uses(ir, copy_id, &spec, source_name)?;
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
    finalize_construction_signature(ir, copy_id, &spec, source_name)?;
    if let Some(record) = ir.specialized_anonymous_classes.get_mut(&copy_id) {
        record.method_clones = clones;
    }
    Ok(())
}

/// Finalize the copied construction only after the streamed class capture prefix is complete.
/// Inline substitution may have specialized the call-site parameter types, while the anonymous
/// class constructor retains its declaration erasure. The construction and declaration must now
/// agree exactly; unlike the earlier copy step, a mismatch here is a malformed final IR state.
fn finalize_construction_signature(
    ir: &mut crate::ir::IrFile,
    copy_id: ClassId,
    spec: &crate::ir::IrSpecializedAnonymousClass,
    source_name: TypeName,
) -> Result<(), super::super::FirLoweringFailure> {
    let declared = ir.classes[copy_id as usize]
        .ctor_args
        .iter()
        .map(|argument| argument.ty)
        .collect::<Vec<_>>();
    let IrExpr::New {
        internal,
        args,
        ctor_params,
        ..
    } = &mut ir.exprs[spec.order as usize]
    else {
        return Err(malformed(source_name));
    };
    if *internal != ir.classes[copy_id as usize].fq_name_id() || args.len() != declared.len() {
        return Err(malformed(source_name));
    }
    if let Some(parameters) = ctor_params {
        if parameters.len() != declared.len() {
            return Err(malformed(source_name));
        }
        *parameters = declared;
    }
    Ok(())
}

/// Property reads inside the copy still name the declaration's property. That layout's owner is
/// the declaration class, and the copy does not inherit it, so a virtual call would target the
/// declaration with the copy as the receiver. The copied accessor is an ordinary method of the copy.
fn retarget_copied_property_calls(
    ir: &mut crate::ir::IrFile,
    copy_id: ClassId,
    source: ClassId,
    source_name: TypeName,
    clones: &HashMap<FunId, FunId>,
) -> Result<(), super::super::FirLoweringFailure> {
    let mut roots = ir.classes[copy_id as usize]
        .methods
        .iter()
        .filter_map(|method| ir.functions.get(*method as usize)?.body)
        .collect::<Vec<_>>();
    roots.extend(ir.classes[copy_id as usize].init_body);
    let mut pending = roots;
    let mut seen = HashSet::new();
    let mut operations = Vec::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        if matches!(
            ir.expr(expression),
            IrExpr::Checked(
                crate::ir::IrCheckedOperation::PropertyRead { .. }
                    | crate::ir::IrCheckedOperation::PropertyWrite { .. }
            )
        ) {
            operations.push(expression);
        }
    }
    for expression in operations {
        retarget_property_operation(ir, expression, copy_id, source, source_name, clones)?;
    }
    Ok(())
}

fn retarget_property_operation(
    ir: &mut crate::ir::IrFile,
    expression: ExprId,
    copy_id: ClassId,
    source: ClassId,
    source_name: TypeName,
    clones: &HashMap<FunId, FunId>,
) -> Result<(), super::super::FirLoweringFailure> {
    let IrExpr::Checked(operation) = ir.expr(expression).clone() else {
        return Ok(());
    };
    let (target, dispatch_receiver, extension_receiver, context_arguments, value) = match operation
    {
        crate::ir::IrCheckedOperation::PropertyRead {
            target,
            dispatch_receiver,
            extension_receiver,
            context_arguments,
            ..
        } => (
            target,
            dispatch_receiver,
            extension_receiver,
            context_arguments,
            None,
        ),
        crate::ir::IrCheckedOperation::PropertyWrite {
            target,
            dispatch_receiver,
            extension_receiver,
            context_arguments,
            value,
            ..
        } => (
            target,
            dispatch_receiver,
            extension_receiver,
            context_arguments,
            Some(value),
        ),
        _ => return Ok(()),
    };
    let Some((getter, setter)) = copied_layout_accessors(ir, target, source, source_name) else {
        return Ok(());
    };
    let function = match value {
        Some(_) => setter.ok_or_else(|| malformed(source_name))?,
        None => getter.ok_or_else(|| malformed(source_name))?,
    };
    let cloned = require_clone(function, clones, source_name)?;
    let index = ir.classes[copy_id as usize]
        .methods
        .iter()
        .position(|method| *method == cloned)
        .ok_or_else(|| malformed(source_name))?;
    let index = u32::try_from(index).map_err(|_| malformed(source_name))?;
    let receiver = dispatch_receiver.ok_or_else(|| malformed(source_name))?;
    let mut args = context_arguments.into_iter().map(Some).collect::<Vec<_>>();
    if let Some(extension_receiver) = extension_receiver {
        args.push(Some(extension_receiver));
    }
    if let Some(value) = value {
        args.push(Some(value));
    }
    ir.exprs[expression as usize] = IrExpr::MethodCall {
        class: copy_id,
        index,
        receiver,
        args,
    };
    Ok(())
}

fn copied_layout_accessors(
    ir: &crate::ir::IrFile,
    property: crate::fir::PropertyId,
    source: ClassId,
    source_name: TypeName,
) -> Option<(Option<FunId>, Option<FunId>)> {
    match ir.local_property_layouts.get(&property)? {
        crate::ir::IrLocalPropertyLayout::Member {
            class,
            owner,
            getter,
            setter,
            ..
        } if *class == source || *owner == source_name => Some((*getter, *setter)),
        crate::ir::IrLocalPropertyLayout::MemberExtension {
            owner,
            getter,
            setter,
            ..
        } if *owner == source_name => Some((Some(*getter), *setter)),
        _ => None,
    }
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
    // Member descriptors stay the declaration's erasure. Reified operations in the body are
    // specialized above; substituting the signature would emit a concrete descriptor plus a
    // bridge where kotlinc keeps the erased accessor.
    shape.dispatch_receiver = Some(dispatch_receiver);
    Ok((
        crate::ir::clone_class_method(ir, method, shape, &HashMap::new()),
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
) -> Result<(), super::super::FirLoweringFailure> {
    let Some(properties) = ir.member_ext_props.get(&source_name).cloned() else {
        return Ok(());
    };
    let mut copied = Vec::with_capacity(properties.len());
    for mut property in properties {
        property.getter = require_clone(property.getter, cloned, source_name)?;
        property.setter =
            mapped_function(property.setter, cloned).map_err(|()| malformed(source_name))?;
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
        if let Some(node) = ir.exprs.get_mut(expression as usize) {
            remap_class(node, from, to, from_name, to_name);
        }
        // A property read records the classifier its receiver statically has. Cloning the
        // expression keeps the declaration class, so the copy would invoke that class's accessor
        // with its own receiver.
        if let Some(owner) = ir.dispatch_classes.get_mut(&expression) {
            if *owner == from_name {
                *owner = to_name;
            }
        }
        if let Some(owner) = ir.expression_owners.get_mut(&expression) {
            if *owner == from_name {
                *owner = to_name;
            }
        }
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

struct InlineLambdaArgument {
    parameter: usize,
    field: u32,
    impl_fn: u32,
    inline_body: ExprId,
    capture_count: usize,
}

/// Constructor arguments of `construction` that pass an inline lambda into a capture of `class`.
fn inline_lambda_arguments(
    ir: &crate::ir::IrFile,
    class: ClassId,
    construction: ExprId,
) -> Vec<InlineLambdaArgument> {
    let IrExpr::New { args, .. } = ir.expr(construction) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (index, argument) in ir.classes[class as usize].ctor_args.iter().enumerate() {
        if argument.provenance != crate::ir::IrCtorParameterProvenance::Capture {
            continue;
        }
        // Local capture prefixes are finalized independently on the declaration and construction.
        // Consume only pairs both sides already publish; exact constructor arity is checked after
        // lowering has completed those lists, not while this streamed inline copy is being taken.
        let Some(actual) = args.get(index).copied() else {
            continue;
        };
        let Some((impl_fn, inline_body, capture_count)) = inline_lambda(ir, actual) else {
            continue;
        };
        let Some(field) = argument.field_index else {
            continue;
        };
        found.push(InlineLambdaArgument {
            parameter: index,
            field,
            impl_fn,
            inline_body,
            capture_count,
        });
    }
    found
}

fn inline_lambda(ir: &crate::ir::IrFile, expression: ExprId) -> Option<(u32, ExprId, usize)> {
    match ir.expr(expression) {
        IrExpr::Lambda {
            impl_fn,
            captures,
            inline_body: Some(inline_body),
            ..
        } => Some((*impl_fn, *inline_body, captures.len())),
        _ => None,
    }
}

/// Inline a lambda into the copy only when its body needs no operand binding or value rehoming.
///
/// Capture freedom alone is not enough: an extension/value parameter still has to be bound, a
/// suspend lambda carries continuation semantics, and a body-local value has to be moved out of the
/// lambda's independent value domain. Those shapes keep the lambda as an initialized constructor
/// capture until the ordinary inline-body splicer can realize them. The narrow shape consumed here
/// is a parameter-free, non-suspend body with no value slots, used exclusively by zero-argument
/// invocations of its capture field.
fn consume_trivially_spliceable_lambdas(
    ir: &mut crate::ir::IrFile,
    class: ClassId,
    construction: ExprId,
    pending_property_roots: &[ExprId],
    lambdas: &[InlineLambdaArgument],
    class_name: TypeName,
) -> Result<u32, super::super::FirLoweringFailure> {
    let mut removals = Vec::new();
    for lambda in lambdas {
        let Some(function) = ir.functions.get(lambda.impl_fn as usize) else {
            return Err(malformed(class_name));
        };
        if lambda.capture_count != 0
            || !function.params.is_empty()
            || ir.suspend_funs.contains(&lambda.impl_fn)
            || body_uses_values(ir, lambda.inline_body)
        {
            continue;
        }
        if !inline_lambda_into_field_invokes(
            ir,
            class,
            pending_property_roots,
            lambda.field,
            lambda.inline_body,
        ) {
            continue;
        }
        ir.inline_only_fns.insert(lambda.impl_fn);
        ir.functions
            .get_mut(lambda.impl_fn as usize)
            .ok_or_else(|| malformed(class_name))?
            .body = None;
        removals.push((lambda.parameter, lambda.field));
    }
    removals.sort_unstable();
    removals.dedup();
    let omitted = u32::try_from(removals.len()).map_err(|_| malformed(class_name))?;
    for (parameter, field) in removals.into_iter().rev() {
        remove_capture(
            ir,
            class,
            construction,
            pending_property_roots,
            parameter,
            field,
            class_name,
        )?;
    }
    Ok(omitted)
}

fn body_uses_values(ir: &crate::ir::IrFile, root: ExprId) -> bool {
    let mut pending = vec![root];
    let mut seen = HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if !super::value_indices(ir.expr(expression)).is_empty() {
            return true;
        }
        if let IrExpr::Lambda { captures, .. } = ir.expr(expression) {
            pending.extend(captures.iter().copied());
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    false
}

fn inline_lambda_into_field_invokes(
    ir: &mut crate::ir::IrFile,
    class: ClassId,
    pending_property_roots: &[ExprId],
    field: u32,
    inline_body: ExprId,
) -> bool {
    let mut roots = ir.classes[class as usize]
        .methods
        .iter()
        .filter_map(|method| ir.functions.get(*method as usize)?.body)
        .collect::<Vec<_>>();
    roots.extend(ir.classes[class as usize].init_body);
    roots.extend(pending_property_roots.iter().copied());
    let mut pending = roots;
    let mut seen = HashSet::new();
    let mut invocations = Vec::new();
    let mut field_reads = HashSet::new();
    let mut invoked_reads = HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::Lambda { .. } = ir.expr(expression) {
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        if let IrExpr::GetField {
            class: owner,
            index,
            ..
        } = ir.expr(expression)
        {
            if *owner == class && *index == field {
                field_reads.insert(expression);
            }
        }
        if let IrExpr::InvokeFunction { func, args, .. } = ir.expr(expression) {
            if let Some(read) = capture_field_read(ir, *func, class, field) {
                if !args.is_empty() {
                    return false;
                }
                invoked_reads.insert(read);
                invocations.push(expression);
            }
        }
    }
    if invocations.is_empty() || field_reads != invoked_reads {
        return false;
    }
    for invocation in invocations {
        let (body, _) = crate::ir::clone_expression_dag(ir, inline_body);
        ir.exprs[invocation as usize] = IrExpr::Block {
            stmts: Vec::new(),
            value: Some(body),
        };
        ir.suspend_calls.remove(&invocation);
        ir.suspend_call_overridden_results.remove(&invocation);
    }
    !capture_field_remains(ir, class, pending_property_roots, field)
}

fn capture_field_read(
    ir: &crate::ir::IrFile,
    expression: ExprId,
    class: ClassId,
    field: u32,
) -> Option<ExprId> {
    match ir.expr(expression) {
        IrExpr::GetField {
            class: owner,
            index,
            ..
        } if *owner == class && *index == field => Some(expression),
        IrExpr::NotNullAssert { operand, .. } => capture_field_read(ir, *operand, class, field),
        _ => None,
    }
}

fn capture_field_remains(
    ir: &crate::ir::IrFile,
    class: ClassId,
    pending_property_roots: &[ExprId],
    field: u32,
) -> bool {
    let mut roots = ir.classes[class as usize]
        .methods
        .iter()
        .filter_map(|method| ir.functions.get(*method as usize)?.body)
        .collect::<Vec<_>>();
    roots.extend(ir.classes[class as usize].init_body);
    roots.extend(pending_property_roots.iter().copied());
    let mut pending = roots;
    let mut seen = HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::Lambda { .. } = ir.expr(expression) {
            continue;
        }
        if let IrExpr::GetField {
            class: owner,
            index,
            ..
        } = ir.expr(expression)
        {
            if *owner == class && *index == field {
                return true;
            }
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    false
}

fn remove_capture(
    ir: &mut crate::ir::IrFile,
    class: ClassId,
    construction: ExprId,
    pending_property_roots: &[ExprId],
    parameter: usize,
    field: u32,
    class_name: TypeName,
) -> Result<(), super::super::FirLoweringFailure> {
    shift_capture_reads(ir, class, pending_property_roots, field);
    let declaration = &mut ir.classes[class as usize];
    if field as usize >= declaration.fields.len()
        || parameter >= declaration.ctor_args.len()
        || declaration.ctor_param_count == 0
        || declaration.constructor_prefix_count == 0
    {
        return Err(malformed(class_name));
    }
    let parameter_index = u32::try_from(parameter).map_err(|_| malformed(class_name))?;
    declaration.fields.remove(field as usize);
    declaration.ctor_param_count -= 1;
    declaration.constructor_prefix_count -= 1;
    for argument in &mut declaration.ctor_args {
        if let Some(index) = argument.field_index.as_mut() {
            if *index > field {
                *index -= 1;
            }
        }
    }
    declaration
        .pre_super_param_fields
        .retain(|(param, stored)| *param != parameter_index && *stored != field);
    for (param, stored) in &mut declaration.pre_super_param_fields {
        if *param > parameter_index {
            *param -= 1;
        }
        if *stored > field {
            *stored -= 1;
        }
    }
    if !declaration.ctor_param_annotations.is_empty() {
        if declaration.ctor_param_annotations.len() != declaration.ctor_args.len() {
            return Err(malformed(class_name));
        }
        declaration.ctor_param_annotations.remove(parameter);
    }
    declaration.ctor_args.remove(parameter);
    shift_capture_map(&mut ir.shared_class_capture_fields, class, field);
    shift_capture_map(&mut ir.class_capture_identities, class, field);
    let IrExpr::New {
        args,
        ctor_params,
        default_prefix_count,
        ..
    } = &mut ir.exprs[construction as usize]
    else {
        return Err(malformed(class_name));
    };
    if parameter >= args.len() {
        return Err(malformed(class_name));
    }
    args.remove(parameter);
    if let Some(parameters) = ctor_params {
        if parameter >= parameters.len() {
            return Err(malformed(class_name));
        }
        parameters.remove(parameter);
    }
    if parameter < *default_prefix_count as usize {
        *default_prefix_count -= 1;
    }
    Ok(())
}

fn shift_capture_reads(
    ir: &mut crate::ir::IrFile,
    class: ClassId,
    pending_property_roots: &[ExprId],
    removed: u32,
) {
    let mut roots = ir.classes[class as usize]
        .methods
        .iter()
        .filter_map(|method| ir.functions.get(*method as usize)?.body)
        .collect::<Vec<_>>();
    roots.extend(ir.classes[class as usize].init_body);
    roots.extend(pending_property_roots.iter().copied());
    let mut pending = roots;
    let mut seen = HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::Lambda { .. } = ir.expr(expression) {
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        if let IrExpr::GetField {
            class: owner,
            index,
            ..
        } = &mut ir.exprs[expression as usize]
        {
            if *owner == class && *index > removed {
                *index -= 1;
            }
        }
    }
}

fn shift_capture_map<V: Clone>(
    map: &mut std::collections::HashMap<(ClassId, u32), V>,
    class: ClassId,
    removed: u32,
) {
    let owned = map
        .iter()
        .filter(|((owner, _), _)| *owner == class)
        .map(|((owner, index), value)| ((*owner, *index), Clone::clone(value)))
        .collect::<Vec<_>>();
    for ((owner, index), _) in &owned {
        map.remove(&(*owner, *index));
    }
    for ((owner, index), value) in owned {
        if index == removed {
            continue;
        }
        let index = if index > removed { index - 1 } else { index };
        map.insert((owner, index), value);
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
    // A class that is itself a copy keeps those bodies on its specialization record.
    roots.extend(property_roots(ir, class));
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
        if node_uses_binding(ir.expr(expression), bindings)
            || super::escaping_lambda::expression_facts_use_binding(ir, expression, bindings)
        {
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
            callee:
                crate::ir::Callee::Intrinsic {
                    operation: crate::ir::IrIntrinsic::TypeOf { ty },
                    ..
                },
            ..
        } => uses(*ty),
        _ => false,
    }
}
