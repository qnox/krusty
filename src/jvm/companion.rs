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
        } if *target == class && *index == field => crate::ir::expr_runs_no_code(ir, *receiver),
        IrExpr::SetField {
            receiver,
            class: target,
            index,
            ..
        } if *target == class && *index == field => crate::ir::expr_runs_no_code(ir, *receiver),
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
        if outer_class.is_interface || !outer_class.enum_entries.is_empty() || outer_class.is_value
        {
            continue;
        }
        let Some(companion) = ir.class_id_by_name(companion_name) else {
            continue;
        };
        let companion_class = &ir.classes[companion as usize];
        if !companion_class.is_companion {
            continue;
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
            if !visibility.is_public()
                || declaration.is_private
                || declaration.is_open
                || declaration.getter.is_some()
                || declaration.setter.is_some()
                || backing.is_lateinit()
                || backing
                    .ty
                    .obj_internal()
                    .is_some_and(|name| ir.is_value_class_name(name))
                || !receiver_is_inert(ir, companion, field)
            {
                continue;
            }
            let Some(initializer_store) = initializer_store(ir, companion, field, initializer)
            else {
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
        ir.declared_class_statics
            .entry(ir.classes[candidate.outer as usize].fq_name)
            .or_default()
            .push(index);
        ir.mark_jvm_companion_hoisted_static(index);
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
                class,
                index,
                value,
                ..
            } if static_for_field.contains_key(&(class, index)) => Some(IrExpr::SetStatic {
                index: static_for_field[&(class, index)],
                value,
            }),
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
    for candidate in candidates {
        let static_index = static_for_field[&(candidate.companion, candidate.field)];
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
        let setter = if candidate.is_var {
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
                // A synthesized setter is still a public Kotlin declaration: a non-null reference
                // parameter gets the same entry guard as an ordinary backend-synthesized setter.
                // The debug-table pass derives its first source PC from this exact prologue.
                param_checks: vec![(candidate.ty.is_reference()
                    && !candidate.ty.is_nullable()
                    && !candidate.ty.is_ty_param())
                .then(|| "<set-?>".to_string())],
            });
            ir.fn_source_order.insert(setter, candidate.source_order);
            Some(setter)
        } else {
            None
        };

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
        class.methods.push(getter);
        if let Some(setter) = setter {
            class.methods.push(setter);
        }
        let property = &mut class.properties[candidate.property];
        property.backing_field = None;
        property.storage_ty = None;
        property.getter = Some(getter);
        property.setter = setter;
    }
}
