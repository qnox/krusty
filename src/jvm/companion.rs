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
    /// A private property has no companion accessor. In-class reads become static reads of the
    /// hoisted field, and another class reaches it through `access$get…$cp`.
    is_private: bool,
    source_order: u32,
    decl_line: u32,
    /// `@JvmField`: the hoisted static IS the property's public surface — a PUBLIC field with no
    /// companion accessors and no `access$…$cp` bridges (kotlinc's realization).
    is_jvm_field: bool,
    /// Accessors the declaration already has. A custom getter or setter stays; only a missing
    /// public accessor is synthesized over the hoisted field.
    declared_getter: Option<u32>,
    declared_setter: Option<u32>,
    /// `x$delegate` storage. Property reads stay on the delegated accessor; the field itself is
    /// still a private static of the outer class.
    delegate_storage: bool,
}

/// The init-body store of a delegated property's `$delegate` field, and the value it stores.
fn delegated_initializer(ir: &IrFile, class: ClassId, field: u32) -> Option<(ExprId, ExprId)> {
    let body = ir.classes[class as usize].init_body?;
    let IrExpr::Block { stmts, value: None } = ir.expr(body) else {
        return None;
    };
    stmts
        .iter()
        .copied()
        .find_map(|statement| match ir.expr(statement) {
            IrExpr::SetField {
                class: target,
                index,
                value,
                ..
            } if *target == class && *index == field => Some((statement, *value)),
            _ => None,
        })
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

fn redundant_companion_receiver(ir: &IrFile, class: ClassId, receiver: ExprId) -> bool {
    let companion = ir.classes[class as usize].fq_name;
    match ir.expr(receiver) {
        IrExpr::SingletonValue { classifier } => *classifier == companion,
        IrExpr::ExternalStaticInstance { ty, .. } => *ty == companion,
        IrExpr::StaticInstance { ty, .. } => ir.classes[*ty as usize].fq_name == companion,
        IrExpr::GetStatic(index) => ir.statics.get(*index as usize).is_some_and(|field| {
            field.ty.obj_internal() == Some(companion)
                || field.ty.non_null().obj_internal() == Some(companion)
        }),
        _ => false,
    }
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
        } if *target == class && *index == field => {
            crate::ir::expr_runs_no_code(ir, *receiver)
                || redundant_companion_receiver(ir, class, *receiver)
        }
        IrExpr::SetField {
            receiver,
            class: target,
            index,
            ..
        } if *target == class && *index == field => {
            crate::ir::expr_runs_no_code(ir, *receiver)
                || redundant_companion_receiver(ir, class, *receiver)
        }
        _ => true,
    })
}

/// Realize the JVM's companion-property storage layout from semantic common IR.
pub(crate) fn lower_companion_properties(
    ir: &mut IrFile,
    realizations: &crate::jvm::property_realizations::PropertyRealizations,
) {
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
        let outer_is_interface = ir.classes.iter().any(|class| {
            class.fq_name_id() == outer && (class.is_interface || class.is_annotation)
        });
        let statics = ir
            .declared_class_statics
            .get(&companion)
            .cloned()
            .unwrap_or_default();
        let mut companion_copies = Vec::new();
        for static_id in statics {
            let declaration = &ir.statics[static_id as usize];
            if !(declaration.is_const && declaration.owner == Some(companion)) {
                continue;
            }
            // A class companion's const moves onto the outer class. An interface companion's
            // public or internal const is stored on both: the interface field is the Java
            // constant, and the companion keeps the same field. A private const cannot be an
            // interface field, so it stays on the companion only.
            if outer_is_interface && !declaration.visibility.is_private() {
                companion_copies.push(declaration.clone());
            }
            if !(outer_is_interface && declaration.visibility.is_private()) {
                ir.statics[static_id as usize].owner = Some(outer);
            }
        }
        ir.statics.extend(companion_copies);
    }

    let mut candidates = Vec::new();
    for outer in 0..ir.classes.len() as ClassId {
        let outer_class = &ir.classes[outer as usize];
        let Some(companion_name) = outer_class.companion_class else {
            continue;
        };
        if outer_class.is_enum || outer_class.is_value {
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
            // A private companion property is hoisted too: kotlinc stores it as a private
            // static of the outer class, not as a companion instance field. An interface
            // companion cannot host that field, and this loop never selects one.
            // A custom getter or setter does not keep the field on the companion. kotlinc
            // still stores it as a private static of the outer class, and the accessor body
            // reads that static through `access$…$cp`.
            // A value-class property is hoisted too. kotlinc stores it as a private static of
            // the outer class — the carrier when that carrier can represent the value, the box
            // when it cannot (`I?` over `Int`). The value-class pass records which of the two
            // this static is; leaving the field on the companion made a property reference call
            // an instance accessor the class does not have.
            if (!visibility.is_public() && !declaration.is_private && !is_jvm_field)
                || declaration.is_open
                || backing.is_lateinit()
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
                is_private: declaration.is_private,
                source_order: declaration.source_order,
                decl_line: declaration.decl_line,
                is_jvm_field,
                declared_getter: declaration.getter,
                declared_setter: declaration.setter,
                delegate_storage: false,
            });
        }
        for property in 0..companion_class.properties.len() {
            let declaration = &companion_class.properties[property];
            let Some(field) = declaration.delegate_field else {
                continue;
            };
            if declaration.is_open || !receiver_is_inert(ir, companion, field) {
                continue;
            }
            let Some(backing) = companion_class.fields.get(field as usize) else {
                continue;
            };
            if backing
                .ty
                .obj_internal()
                .is_some_and(|name| crate::jvm::value_classes::is_boxed_value_class(ir, name))
            {
                continue;
            }
            let Some((initializer_store, initializer)) =
                delegated_initializer(ir, companion, field)
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
                name: backing.name.clone(),
                ty: backing.ty,
                is_var: false,
                visibility: Visibility::Private,
                is_private: true,
                source_order: declaration.source_order,
                decl_line: declaration.decl_line,
                is_jvm_field: false,
                declared_getter: declaration.getter,
                declared_setter: declaration.setter,
                delegate_storage: true,
            });
        }
    }
    if candidates.is_empty() {
        schedule_class_companion_initializers(ir, realizations);
        return;
    }

    let mut static_for_field = HashMap::new();
    for candidate in &candidates {
        let index = ir.statics.len() as u32;
        ir.statics.push(IrStatic {
            name: candidate.name.clone(),
            ty: candidate.ty,
            init: Some(candidate.initializer),
            is_var: candidate.is_var,
            is_const: false,
            is_lateinit: false,
            owner: Some(ir.classes[candidate.outer as usize].fq_name),
            visibility: candidate.visibility,
            setter_jvm_name: None,
            erased_declared_ty: None,
            accessors: crate::ir::IrStaticAccessors::DEFAULT,
            line: candidate.decl_line,
            source_order: candidate.source_order,
        });
        // Deliberately NOT registered in `declared_class_statics`: that table feeds the owner's
        // `@Metadata` const-property records, and kotlinc's owner metadata has NO record for a
        // hoisted companion property (the declaration belongs to the companion's metadata alone).
        // Emission finds the physical field through `IrStatic.owner`.
        ir.mark_jvm_companion_hoisted_static(index);
        // The outer class's own static fields keep their names; a hoisted companion property that
        // meets one (a same-named `companion { … }` property) takes the next free `$N` suffix, as
        // kotlinc names it. Its accessors and bridges keep the property's name.
        let owner = ir.classes[candidate.outer as usize].fq_name;
        let descriptor = crate::jvm::names::type_descriptor(candidate.ty);
        let occupied = |ir: &IrFile, name: &str| {
            ir.statics.iter().enumerate().any(|(other, property)| {
                other as u32 != index
                    && property.owner == Some(owner)
                    && ir.static_field_jvm_name(other as u32) == name
                    && crate::jvm::names::type_descriptor(property.ty) == descriptor
            })
        };
        if occupied(ir, &candidate.name) {
            let physical = (1usize..)
                .map(|suffix| format!("{}${suffix}", candidate.name))
                .find(|physical| !occupied(ir, physical))
                .expect("an unused JVM static field suffix always exists");
            ir.set_jvm_static_field_name(index, physical);
        }
        if candidate.is_jvm_field {
            ir.mark_jvm_field_static(index);
        }
        // A delegate field is not the property. Reads of `secret` stay on `getSecret`, which
        // then loads `secret$delegate`. Recording the delegate as the property static would
        // bypass that accessor.
        if !candidate.delegate_storage {
            ir.mark_jvm_companion_property_static(
                ir.classes[candidate.companion as usize].fq_name,
                candidate.property as u32,
                index,
            );
        }
        static_for_field.insert((candidate.companion, candidate.field), index);
    }

    // Remove only declaration-initializer stores. A later assignment in an `init` block has a
    // different expression identity and remains in source order (rewritten to the selected static).
    // When the body also has an `init` block (or any other statement), the stores stay: the outer
    // `<clinit>` runs that whole body in source order, and each hoisted static's own initializer
    // is cleared so the value is not stored twice.
    let removed_stores: HashSet<_> = candidates
        .iter()
        .map(|candidate| candidate.initializer_store)
        .collect();
    let affected_companions: HashSet<_> = candidates
        .iter()
        .map(|candidate| candidate.companion)
        .collect();
    let mut retain_initializer_stores = HashSet::new();
    for companion in affected_companions.iter().copied() {
        let Some(body) = ir.classes[companion as usize].init_body else {
            continue;
        };
        let IrExpr::Block { stmts, value } = ir.expr(body).clone() else {
            continue;
        };
        let has_other_statement = value.is_some()
            || stmts
                .iter()
                .any(|statement| !removed_stores.contains(statement));
        // Hoisted stores become outer-class static stores. When the body also has an `init`
        // block and every field it touches is moving with it, keep those stores in the body so
        // `<clinit>` runs one source-ordered initializer after the companion instance is stored.
        // A field that is not moving (an open property, a lateinit, a boxed value class)
        // stays private to the companion, so an initializer that still touches it stays on
        // the constructor. A custom accessor or a delegate moves with the other fields, and
        // the body stays one source-ordered initializer.
        let hoisted_fields = candidates
            .iter()
            .filter(|candidate| candidate.companion == companion)
            .map(|candidate| candidate.field)
            .collect::<HashSet<_>>();
        if has_other_statement
            && class_companion_clinit_owner(ir, companion).is_some()
            && !initializer_needs_companion_instance(
                ir,
                companion,
                body,
                &removed_stores,
                &hoisted_fields,
            )
        {
            retain_initializer_stores.insert(companion);
            continue;
        }
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
                // The companion instance is already stored. Reloading it only to discard it
                // before a static store is not part of kotlinc's `<clinit>`.
                if crate::ir::expr_runs_no_code(ir, receiver)
                    || redundant_companion_receiver(ir, class, receiver)
                {
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
    retarget_surviving_field_indices(ir, &remaps);

    // Build the companion's ordinary accessor declarations over the selected static realization.
    // A `@JvmField` property gets NONE: the public owner field is its entire JVM surface.
    for candidate in candidates {
        let static_index = static_for_field[&(candidate.companion, candidate.field)];
        // A private property has no getter or setter. kotlinc reads the hoisted field
        // directly from the outer class and through `access$…$cp` from everywhere else.
        // A declared accessor stays. Only the side the source omitted is synthesized, so a
        // custom setter still gets the default getter over the hoisted field.
        let accessors = (!candidate.is_jvm_field && !candidate.is_private).then(|| {
            let getter = candidate.declared_getter.is_none().then(|| {
                let getter_name = property_getter_name(&candidate.name);
                let read = ir.add_expr(IrExpr::GetStatic(static_index));
                let returned = ir.add_expr(IrExpr::Return(Some(read)));
                let getter_body = ir.add_expr(IrExpr::Block {
                    stmts: vec![returned],
                    value: None,
                });
                let getter = ir.add_fun(IrFunction {
                    name: getter_name.clone(),
                    params: vec![],
                    ret: candidate.ty,
                    body: Some(getter_body),
                    is_static: false,
                    dispatch_receiver: Some(ir.classes[candidate.companion as usize].fq_name),
                    param_checks: vec![],
                });
                ir.fn_source_names.insert(getter, getter_name);
                ir.fn_params
                    .insert(getter, crate::ir::FnParamInfo::identities(Vec::new()));
                ir.fn_source_order.insert(getter, candidate.source_order);
                getter
            });
            let setter = (candidate.is_var && candidate.declared_setter.is_none()).then(|| {
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
                    .then_some(crate::ir::IrParameterCheck::NonNull)],
                });
                ir.fn_source_names
                    .insert(setter, property_setter_name(&candidate.name));
                ir.fn_params.insert(
                    setter,
                    crate::ir::FnParamInfo::identities(vec![
                        crate::ir::IrParameterIdentity::property_setter_value(),
                    ]),
                );
                ir.fn_source_order.insert(setter, candidate.source_order);
                // kotlinc writes the setter's `<set-?>` local with the setter, before the next
                // member, so its table is recorded as the body is emitted.
                ir.fn_debug_locals.insert(setter);
                setter
            });
            (getter, setter)
        });

        let mut initializer = candidate.initializer;
        if retain_initializer_stores.contains(&candidate.companion) {
            // The store stays in the companion body, which `<clinit>` runs with the instance
            // already in value 0. A separate static initializer would store the value again.
            ir.statics[static_index as usize].init = None;
        } else {
            if crate::ir::read_values(ir, initializer).contains(&0) {
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
            ir.statics[static_index as usize].init = Some(initializer);
        }

        let class = &mut ir.classes[candidate.companion as usize];
        let property_accessors = accessors.map(|(getter, setter)| {
            if let Some(getter) = getter {
                class.methods.push(getter);
            }
            if let Some(setter) = setter {
                class.methods.push(setter);
            }
            (getter, setter)
        });
        let property = &mut class.properties[candidate.property];
        property.backing_field = None;
        property.storage_ty = None;
        if let Some((getter, setter)) = property_accessors {
            if let Some(getter) = getter {
                property.getter = Some(getter);
            }
            if let Some(setter) = setter {
                property.setter = Some(setter);
            }
        }
    }
    schedule_class_companion_initializers(ir, realizations);
}

/// A class companion's initializer runs in the enclosing class `<clinit>`, after that class has
/// stored the companion instance. Property initializers and `init` blocks stay in source order
/// inside the body. The companion constructor is left as `super()` only.
///
/// Interface companions keep object storage and run their initializer in their own `<clinit>`.
/// Enum companions stay on the enum's existing `<clinit>` order. A named object's companion stays
/// with that object's initializer.
fn schedule_class_companion_initializers(
    ir: &mut IrFile,
    realizations: &crate::jvm::property_realizations::PropertyRealizations,
) {
    let mut scheduled = Vec::new();
    for index in 0..ir.classes.len() {
        if !outer_runs_companion_initializer(&ir.classes[index]) {
            continue;
        }
        let Some(companion_name) = ir.classes[index].companion_class else {
            continue;
        };
        let Some(companion) = ir.class_id_by_name(companion_name) else {
            continue;
        };
        if !ir.classes[companion as usize].is_companion {
            continue;
        }
        let Some(body) = ir.classes[companion as usize].init_body else {
            continue;
        };
        // Hoisted fields are already static reads. What remains is a companion instance field,
        // which the outer class cannot touch. That initializer stays on the constructor.
        if initializer_needs_companion_instance(
            ir,
            companion,
            body,
            &HashSet::new(),
            &HashSet::new(),
        ) {
            continue;
        }
        scheduled.push((ir.classes[index].fq_name, companion, body));
    }
    for (owner, companion, body) in scheduled {
        // The initializer now emits in the outer `<clinit>`. A lambda it evaluates is a private
        // static of that class; leaving it on the companion makes the outer class call a private
        // method it cannot see.
        move_initializer_lambdas(ir, companion, owner, body);
        // `<clinit>` has no `this`. Each use of the companion instance reloads the outer field.
        // A single local would be `astore`/`aload`, which is not kotlinc's `<clinit>`.
        reload_companion_instance(ir, owner, companion, body);
        // The moved body calls the companion's private accessors. The property operation
        // already carries its declaration identity; record that accessor on the operation.
        record_moved_initializer_accessors(ir, body, realizations);
        ir.set_companion_clinit_body(owner, body);
        ir.classes[companion as usize].init_body = None;
    }
}

/// Move lambda implementation methods used by `body` from the companion onto `outer`.
///
/// Lowering attaches each lambda to the companion, the class whose constructor originally held
/// the initializer. After the body moves, the `invokedynamic` is emitted by the outer class.
/// Replace companion-`this` reads in `body` with a load of the outer `Companion` field.
fn reload_companion_instance(
    ir: &mut IrFile,
    outer: crate::types::TypeName,
    companion: ClassId,
    body: ExprId,
) {
    let companion_name = ir.classes[companion as usize].fq_name;
    let field = companion_name.nested_segment_ref().to_string();
    let mut pending = vec![body];
    let mut seen = HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if matches!(ir.expr(expression), IrExpr::GetValue(0)) {
            ir.exprs[expression as usize] = IrExpr::ExternalStaticInstance {
                owner: outer,
                ty: companion_name,
                field: field.clone(),
            };
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
}

/// Bind a private getter or setter to the property operation that calls it.
///
/// Realization has already replaced the checked operation with a property read or write and
/// kept the operation id. That id selects the declaration. Bridge collection reads the
/// accessor stored here. A missing realization or accessor stays missing.
fn record_moved_initializer_accessors(
    ir: &mut IrFile,
    body: ExprId,
    realizations: &crate::jvm::property_realizations::PropertyRealizations,
) {
    let mut pending = vec![body];
    let mut seen = HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        let lambda = match ir.expr(expression) {
            IrExpr::Lambda { impl_fn, .. } => Some(*impl_fn),
            _ => None,
        };
        if let Some(function) = lambda {
            if let Some(nested) = ir
                .functions
                .get(function as usize)
                .and_then(|function| function.body)
            {
                pending.push(nested);
            }
        }
        if let Some((operation, target, read)) = moved_property_use(ir, expression, realizations) {
            if let Some(function) = private_declared_accessor(ir, target, read) {
                ir.jvm_member_targets.insert(operation, function);
            }
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
}

/// The declaration a moved property read or write already selected.
///
/// A property read carries the operation id realization recorded. A checked operation that
/// has not been realized yet still carries the property id itself. Neither shape is recovered
/// from the property's source spelling.
fn moved_property_use(
    ir: &IrFile,
    expression: ExprId,
    realizations: &crate::jvm::property_realizations::PropertyRealizations,
) -> Option<(ExprId, crate::fir::PropertyId, bool)> {
    match ir.expr(expression) {
        IrExpr::PropertyRead { operation, .. } => {
            let operation = operation.unwrap_or(expression);
            Some((operation, local_property(realizations, operation)?, true))
        }
        IrExpr::PropertyWrite { operation, .. } => {
            let operation = operation.unwrap_or(expression);
            Some((operation, local_property(realizations, operation)?, false))
        }
        IrExpr::Checked(crate::ir::IrCheckedOperation::PropertyRead { target, .. }) => {
            Some((expression, *target, true))
        }
        IrExpr::Checked(crate::ir::IrCheckedOperation::PropertyWrite { target, .. }) => {
            Some((expression, *target, false))
        }
        _ => None,
    }
}

fn local_property(
    realizations: &crate::jvm::property_realizations::PropertyRealizations,
    operation: ExprId,
) -> Option<crate::fir::PropertyId> {
    match realizations.get(operation)? {
        crate::jvm::property_realizations::PropertyRealization::Local(target) => Some(*target),
        crate::jvm::property_realizations::PropertyRealization::Physical(_) => None,
    }
}

fn private_declared_accessor(
    ir: &IrFile,
    target: crate::fir::PropertyId,
    read: bool,
) -> Option<u32> {
    let crate::ir::IrLocalPropertyLayout::Member {
        class, property, ..
    } = ir.local_property_layouts.get(&target)?
    else {
        return None;
    };
    let declaration = ir
        .classes
        .get(*class as usize)?
        .properties
        .get(*property as usize)?;
    let function = if read {
        declaration.getter
    } else {
        declaration.setter
    }?;
    ir.method_visibility(function)
        .is_private()
        .then_some(function)
}

fn move_initializer_lambdas(
    ir: &mut IrFile,
    companion: ClassId,
    outer: crate::types::TypeName,
    body: ExprId,
) {
    let Some(outer_class) = ir.class_id_by_name(outer) else {
        return;
    };
    let mut pending = vec![body];
    let mut seen = HashSet::new();
    let mut lambdas = Vec::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::Lambda { impl_fn, .. } = ir.expr(expression) {
            lambdas.push(*impl_fn);
            if let Some(nested) = ir
                .functions
                .get(*impl_fn as usize)
                .and_then(|function| function.body)
            {
                pending.push(nested);
            }
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    let moving: Vec<u32> = lambdas
        .into_iter()
        .filter(|function| ir.classes[companion as usize].methods.contains(function))
        .collect();
    if moving.is_empty() {
        return;
    }
    ir.classes[companion as usize]
        .methods
        .retain(|method| !moving.contains(method));
    for function in &moving {
        let methods = &mut ir.classes[outer_class as usize].methods;
        if !methods.contains(function) {
            methods.push(*function);
        }
    }
    let owner = ir.classes[outer_class as usize].fq_name_id();
    for function in moving {
        ir.class_static_local_functions.insert(function, owner);
    }
}

/// Whether `body` still reads or writes a companion instance field.
///
/// `hoisted_fields` are backing fields about to become outer statics. Their operations do not
/// keep the initializer on the constructor: after the rewrite they are static loads of the
/// outer class. A field that stays on the companion is private there, so the outer `<clinit>`
/// cannot load it.
fn initializer_needs_companion_instance(
    ir: &IrFile,
    companion: ClassId,
    body: ExprId,
    skip: &HashSet<ExprId>,
    hoisted_fields: &HashSet<u32>,
) -> bool {
    let companion_name = ir.classes[companion as usize].fq_name;
    let mut pending = vec![body];
    let mut seen = HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) || skip.contains(&expression) {
            continue;
        }
        match ir.expr(expression) {
            IrExpr::GetField { class, index, .. }
            | IrExpr::SetField { class, index, .. }
            | IrExpr::LateinitInitialized { class, index, .. }
                if *class == companion && !hoisted_fields.contains(index) =>
            {
                return true;
            }
            IrExpr::PropertyRead { owner, name, .. }
            | IrExpr::PropertyWrite { owner, name, .. }
                if *owner == companion_name
                    && ir.classes[companion as usize]
                        .properties
                        .iter()
                        .any(|property| {
                            property.name == *name
                                && property
                                    .backing_field
                                    .is_some_and(|field| !hoisted_fields.contains(&field))
                        }) =>
            {
                return true;
            }
            _ => {}
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    false
}

fn outer_runs_companion_initializer(outer: &crate::ir::IrClass) -> bool {
    !outer.is_interface
        && !outer.is_annotation
        && !outer.is_enum
        && !outer.is_value
        && !outer.is_object
        && outer.companion_class.is_some()
}

/// Outer class that will run `companion`'s initializer, when `companion` is that kind of companion.
fn class_companion_clinit_owner(ir: &IrFile, companion: ClassId) -> Option<ClassId> {
    let companion_name = ir.classes.get(companion as usize)?.fq_name;
    if !ir
        .classes
        .get(companion as usize)
        .is_some_and(|class| class.is_companion)
    {
        return None;
    }
    ir.classes
        .iter()
        .position(|class| {
            class.companion_class == Some(companion_name) && outer_runs_companion_initializer(class)
        })
        .map(|index| index as ClassId)
}

/// A property that stays on the companion still names its field after plainer siblings are removed.
///
/// Hoisting deletes those siblings' fields and compacts the table. Expression field operations are
/// retargeted with the table; the declaration's own field index is a separate identity and moves
/// with them. A `var` that only customizes its setter synthesizes its default getter from that
/// index, so the getter still addresses the field the setter writes.
fn retarget_surviving_field_indices(ir: &mut IrFile, remaps: &HashMap<ClassId, Vec<Option<u32>>>) {
    for (&class, remap) in remaps {
        for property in &mut ir.classes[class as usize].properties {
            property.backing_field = retarget_field_index(remap, property.backing_field);
            property.delegate_field = retarget_field_index(remap, property.delegate_field);
        }
        let owner = ir.classes[class as usize].fq_name;
        if let Some(properties) = ir.member_ext_props.get_mut(&owner) {
            for property in properties {
                property.delegate_field = retarget_field_index(remap, property.delegate_field);
            }
        }
    }
    for layout in ir.local_property_layouts.values_mut() {
        if let crate::ir::IrLocalPropertyLayout::Member {
            class,
            backing_field,
            ..
        } = layout
        {
            if let Some(remap) = remaps.get(class) {
                *backing_field = retarget_field_index(remap, *backing_field);
            }
        }
    }
}

fn retarget_field_index(remap: &[Option<u32>], index: Option<u32>) -> Option<u32> {
    index.and_then(|index| remap[index as usize])
}
