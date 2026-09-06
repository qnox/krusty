//! JVM storage realization for ordinary companion-object properties.
//!
//! Common IR keeps a companion property as an instance declaration with a semantic initializer.
//! kotlinc's JVM layout moves a supported plain property's backing storage to the outer class and
//! realizes the companion accessors through synthetic outer bridges. This pass performs that physical
//! rewrite after common lowering; it never re-reads the AST or resolves a declaration from its name.

use std::collections::{HashMap, HashSet};

use crate::ir::{ClassId, ExprId, IrExpr, IrFile, IrFunction, IrStatic};
use crate::jvm::names::{property_getter_name, property_setter_name};
use crate::types::{Ty, Visibility};

#[derive(Clone)]
struct Candidate {
    outer: ClassId,
    companion: ClassId,
    property: usize,
    field: u32,
    initializer: ExprId,
    initializer_store: ExprId,
    name: String,
    ty: Ty,
    is_var: bool,
    visibility: Visibility,
    source_order: u32,
    decl_line: u32,
    /// `@JvmField`: the hoisted static IS the property's public surface — a PUBLIC field with no
    /// companion accessors and no `access$…$cp` bridges (kotlinc's realization).
    is_jvm_field: bool,
}

fn initializer_store(
    ir: &IrFile,
    class: ClassId,
    field: u32,
    initializer: ExprId,
) -> Option<ExprId> {
    let body = ir.classes[class as usize].init_body?;
    let IrExpr::Block { stmts, value: None } = ir.expr(body) else {
        return None;
    };
    stmts.iter().copied().find(|&statement| {
        matches!(
            ir.expr(statement),
            IrExpr::SetField {
                class: target,
                index,
                value,
                ..
            } if *target == class && *index == field && *value == initializer
        )
    })
}

fn receiver_is_inert(ir: &IrFile, class: ClassId, field: u32) -> bool {
    let receiver_is_redundant_companion_value = |receiver: ExprId| {
        let companion = ir.classes[class as usize].fq_name;
        match ir.expr(receiver) {
            IrExpr::SingletonValue { classifier } => *classifier == companion,
            IrExpr::ExternalStaticInstance { ty, .. } => *ty == companion,
            IrExpr::StaticInstance { ty, .. } => ir.classes[*ty as usize].fq_name == companion,
            _ => false,
        }
    };
    ir.exprs.iter().all(|expression| match expression {
        IrExpr::GetField {
            receiver,
            class: target,
            index,
        }
        | IrExpr::LateinitInitialized {
            receiver,
            class: target,
            index,
        } if *target == class && *index == field => {
            crate::ir::expr_runs_no_code(ir, *receiver)
                || receiver_is_redundant_companion_value(*receiver)
        }
        IrExpr::SetField {
            receiver,
            class: target,
            index,
            ..
        } if *target == class && *index == field => {
            crate::ir::expr_runs_no_code(ir, *receiver)
                || receiver_is_redundant_companion_value(*receiver)
        }
        _ => true,
    })
}

fn reads_value(ir: &IrFile, root: ExprId, value: u32) -> bool {
    let mut pending = vec![root];
    let mut seen = HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if matches!(ir.expr(expression), IrExpr::GetValue(found) if *found == value) {
            return true;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    false
}

/// Realize the JVM's companion-property storage layout from semantic common IR.
pub fn lower_companion_properties(ir: &mut IrFile) {
    // A companion `const val` remains declared by the companion in common IR and metadata, while
    // the JVM stores its public static final field on the outer class. Select that physical owner by
    // exact class/static identities here; no declaration is rebound from its spelling.
    let companion_owners: Vec<_> = ir
        .classes
        .iter()
        .filter_map(|outer| {
            outer
                .companion_class
                .map(|companion| (outer.fq_name, companion))
        })
        .collect();
    for (outer, companion) in companion_owners {
        let statics = ir
            .declared_class_statics
            .get(&companion)
            .cloned()
            .unwrap_or_default();
        for static_id in statics {
            let declaration = &mut ir.statics[static_id as usize];
            if declaration.is_const && declaration.owner == Some(companion) {
                declaration.owner = Some(outer);
            }
        }
    }

    let mut candidates = Vec::new();
    for outer in 0..ir.classes.len() as ClassId {
        let outer_class = &ir.classes[outer as usize];
        let Some(companion_name) = outer_class.companion_class else {
            continue;
        };
        if !outer_class.enum_entries.is_empty() || outer_class.is_value {
            continue;
        }
        // Annotation classes are distinct in common IR but are JVM interfaces for storage rules.
        let outer_is_interface = outer_class.is_interface || outer_class.is_annotation;
        let Some(companion) = ir.class_id_by_name(companion_name) else {
            continue;
        };
        let companion_class = &ir.classes[companion as usize];
        if !companion_class.is_companion {
            continue;
        }
        // Whether this property is a `@JvmField`-realizable declaration: annotated, ordinary
        // storage (no custom accessor, not lateinit/open), and at least internal-visible — kotlinc
        // rejects the other placements outright, so an annotated-but-ineligible property simply
        // keeps the ordinary realization here.
        let jvm_field_eligible = |declaration: &crate::ir::IrProperty,
                                  backing: &crate::ir::IrField| {
            companion_class.property_has_jvm_field(&declaration.name)
                && matches!(
                    declaration.visibility,
                    Visibility::Public | Visibility::Internal
                )
                && !declaration.is_private
                && !declaration.is_open
                && declaration.getter.is_none()
                && declaration.setter.is_none()
                && !backing.is_lateinit()
        };
        // An INTERFACE owner admits `@JvmField` hoisting only under kotlinc's whole-companion rule:
        // every companion property is a `public final val` with `@JvmField` (an interface field is
        // forced `public static final`, so nothing else has a legal realization there). Ordinary
        // (non-`@JvmField`) interface-companion properties keep object-style storage on the
        // companion itself. The rule spans the companion's WHOLE property universe: a `const val`
        // is not in `properties` (its declaration is a class static), but it is a companion
        // property all the same. The frontend rejects a non-uniform source declaration; this
        // backend-side guard keeps malformed or legacy common IR from selecting an impossible
        // interface field layout.
        if outer_is_interface {
            let has_const_statics = ir
                .declared_class_statics
                .get(&companion_name)
                .is_some_and(|statics| !statics.is_empty());
            let all_jvm_field_vals = !has_const_statics
                && !companion_class.properties.is_empty()
                && companion_class.properties.iter().all(|declaration| {
                    declaration
                        .backing_field
                        .and_then(|field| companion_class.fields.get(field as usize))
                        .is_some_and(|backing| {
                            jvm_field_eligible(declaration, backing)
                                && !declaration.is_var
                                && declaration.visibility.is_public()
                        })
                });
            if !all_jvm_field_vals {
                continue;
            }
        }
        for property in 0..companion_class.properties.len() {
            let declaration = &companion_class.properties[property];
            let Some(field) = declaration.backing_field else {
                continue;
            };
            let Some(initializer) = declaration.initializer else {
                continue;
            };
            let backing = &companion_class.fields[field as usize];
            let visibility = declaration.visibility;
            let is_jvm_field = jvm_field_eligible(declaration, backing);
            let inert_receiver = receiver_is_inert(ir, companion, field);
            let initializer_store = initializer_store(ir, companion, field, initializer);
            if (!visibility.is_public() && !is_jvm_field)
                || declaration.is_private
                || declaration.is_open
                || declaration.getter.is_some()
                || declaration.setter.is_some()
                || backing.is_lateinit()
                || backing
                    .ty
                    .obj_internal()
                    .is_some_and(|name| ir.is_value_class_name(name))
                || (!is_jvm_field && !inert_receiver)
            {
                continue;
            }
            let Some(initializer_store) = initializer_store else {
                continue;
            };
            candidates.push(Candidate {
                outer,
                companion,
                property,
                field,
                initializer,
                initializer_store,
                name: declaration.name.clone(),
                ty: declaration.ty,
                is_var: declaration.is_var,
                visibility,
                source_order: declaration.source_order,
                decl_line: declaration.decl_line,
                is_jvm_field,
            });
        }
    }
    if candidates.is_empty() {
        return;
    }

    let mut static_for_field = HashMap::new();
    for candidate in &candidates {
        let index = ir.statics.len() as u32;
        ir.statics.push(IrStatic {
            name: candidate.name.clone(),
            ty: candidate.ty,
            init: candidate.initializer,
            is_var: candidate.is_var,
            is_const: false,
            owner: Some(ir.classes[candidate.outer as usize].fq_name),
            visibility: candidate.visibility,
            custom_accessor: false,
            line: candidate.decl_line,
            source_order: candidate.source_order,
        });
        // Deliberately NOT registered in `declared_class_statics`: that table feeds the owner's
        // `@Metadata` const-property records, and kotlinc's owner metadata has NO record for a
        // hoisted companion property (the declaration belongs to the companion's metadata alone).
        // Emission finds the physical field through `IrStatic.owner`.
        ir.mark_jvm_companion_hoisted_static(index);
        if candidate.is_jvm_field {
            ir.mark_jvm_field_static(index);
        }
        ir.mark_jvm_companion_property_static(
            ir.classes[candidate.companion as usize].fq_name,
            candidate.property as u32,
            index,
        );
        static_for_field.insert((candidate.companion, candidate.field), index);
    }

    // Remove only declaration-initializer stores. A later assignment in an `init` block has a
    // different expression identity and remains in source order (rewritten to the selected static).
    let removed_stores: HashSet<_> = candidates
        .iter()
        .map(|candidate| candidate.initializer_store)
        .collect();
    let affected_companions: HashSet<_> = candidates
        .iter()
        .map(|candidate| candidate.companion)
        .collect();
    for companion in affected_companions.iter().copied() {
        let Some(body) = ir.classes[companion as usize].init_body else {
            continue;
        };
        let IrExpr::Block { stmts, value } = ir.expr(body).clone() else {
            continue;
        };
        let retained: Vec<_> = stmts
            .into_iter()
            .filter(|statement| !removed_stores.contains(statement))
            .collect();
        if retained.is_empty() && value.is_none() {
            ir.classes[companion as usize].init_body = None;
        } else {
            ir.exprs[body as usize] = IrExpr::Block {
                stmts: retained,
                value,
            };
        }
    }

    // Rewrite every already-lowered access by stable class/field identity before compacting fields.
    let original_expression_count = ir.exprs.len();
    for expression in 0..original_expression_count {
        let replacement = match ir.exprs[expression].clone() {
            IrExpr::GetField { class, index, .. }
                if static_for_field.contains_key(&(class, index)) =>
            {
                Some(IrExpr::GetStatic(static_for_field[&(class, index)]))
            }
            IrExpr::SetField {
                receiver,
                class,
                index,
                value,
            } if static_for_field.contains_key(&(class, index)) => {
                let write = IrExpr::SetStatic {
                    index: static_for_field[&(class, index)],
                    value,
                };
                if crate::ir::expr_runs_no_code(ir, receiver) {
                    Some(write)
                } else {
                    let write = ir.add_expr(write);
                    Some(IrExpr::Block {
                        stmts: vec![receiver, write],
                        value: None,
                    })
                }
            }
            _ => None,
        };
        if let Some(replacement) = replacement {
            ir.exprs[expression] = replacement;
        }
    }

    // Compact each companion's field table and retarget all surviving field identities.
    let mut remaps = HashMap::<ClassId, Vec<Option<u32>>>::new();
    for companion in affected_companions.iter().copied() {
        let fields = std::mem::take(&mut ir.classes[companion as usize].fields);
        let mut retained = Vec::with_capacity(fields.len());
        let mut remap = Vec::with_capacity(fields.len());
        for (old, field) in fields.into_iter().enumerate() {
            if static_for_field.contains_key(&(companion, old as u32)) {
                remap.push(None);
            } else {
                remap.push(Some(retained.len() as u32));
                retained.push(field);
            }
        }
        ir.classes[companion as usize].fields = retained;
        remaps.insert(companion, remap);
    }
    for expression in &mut ir.exprs {
        match expression {
            IrExpr::GetField { class, index, .. }
            | IrExpr::SetField { class, index, .. }
            | IrExpr::LateinitInitialized { class, index, .. } => {
                if let Some(remap) = remaps.get(class) {
                    *index = remap[*index as usize]
                        .expect("hoisted field operation rewritten before compaction");
                }
            }
            _ => {}
        }
    }

    // Build the companion's ordinary accessor declarations over the selected static realization.
    // A `@JvmField` property gets NONE: the public owner field is its entire JVM surface.
    for candidate in candidates {
        let static_index = static_for_field[&(candidate.companion, candidate.field)];
        let accessors = (!candidate.is_jvm_field).then(|| {
            let getter_name = property_getter_name(&candidate.name);
            let read = ir.add_expr(IrExpr::GetStatic(static_index));
            let returned = ir.add_expr(IrExpr::Return(Some(read)));
            let getter_body = ir.add_expr(IrExpr::Block {
                stmts: vec![returned],
                value: None,
            });
            let getter = ir.add_fun(IrFunction {
                name: getter_name,
                params: vec![],
                ret: candidate.ty,
                body: Some(getter_body),
                is_static: false,
                dispatch_receiver: Some(ir.classes[candidate.companion as usize].fq_name),
                param_checks: vec![],
            });
            ir.fn_source_order.insert(getter, candidate.source_order);
            let setter = candidate.is_var.then(|| {
                let value = ir.add_expr(IrExpr::GetValue(1));
                let write = ir.add_expr(IrExpr::SetStatic {
                    index: static_index,
                    value,
                });
                let returned = ir.add_expr(IrExpr::Return(None));
                let body = ir.add_expr(IrExpr::Block {
                    stmts: vec![write, returned],
                    value: None,
                });
                let setter = ir.add_fun(IrFunction {
                    name: property_setter_name(&candidate.name),
                    params: vec![candidate.ty],
                    ret: Ty::Unit,
                    body: Some(body),
                    is_static: false,
                    dispatch_receiver: Some(ir.classes[candidate.companion as usize].fq_name),
                    // A synthesized setter is still a public Kotlin declaration: a non-null
                    // reference parameter gets the same entry guard as an ordinary
                    // backend-synthesized setter. The debug-table pass derives its first source PC
                    // from this exact prologue.
                    param_checks: vec![(candidate.ty.is_reference()
                        && !candidate.ty.is_nullable()
                        && !candidate.ty.is_ty_param())
                    .then(|| "<set-?>".to_string())],
                });
                ir.fn_source_order.insert(setter, candidate.source_order);
                setter
            });
            (getter, setter)
        });

        let mut initializer = candidate.initializer;
        if reads_value(ir, initializer, 0) {
            let outer = ir.classes[candidate.outer as usize].fq_name;
            let companion = ir.classes[candidate.companion as usize].fq_name;
            let singleton = ir.add_expr(IrExpr::ExternalStaticInstance {
                owner: outer,
                ty: companion,
                field: companion.nested_segment_ref().to_string(),
            });
            let binding = ir.add_expr(IrExpr::Variable {
                index: 0,
                ty: Ty::obj_name(companion),
                init: Some(singleton),
                named: false,
            });
            initializer = ir.add_expr(IrExpr::Block {
                stmts: vec![binding],
                value: Some(initializer),
            });
        }
        ir.statics[static_index as usize].init = initializer;

        let class = &mut ir.classes[candidate.companion as usize];
        let property_accessors = accessors.map(|(getter, setter)| {
            class.methods.push(getter);
            if let Some(setter) = setter {
                class.methods.push(setter);
            }
            (getter, setter)
        });
        let property = &mut class.properties[candidate.property];
        property.backing_field = None;
        property.storage_ty = None;
        if let Some((getter, setter)) = property_accessors {
            property.getter = Some(getter);
            property.setter = setter;
        }
    }
}
