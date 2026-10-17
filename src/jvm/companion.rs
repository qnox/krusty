//! JVM storage realization for ordinary companion-object properties.
//!
//! Common IR keeps a companion property as an instance declaration with a semantic initializer.
//! kotlinc's JVM layout moves a supported plain property's backing storage to the outer class and
//! realizes the companion accessors through synthetic outer bridges. This pass performs that physical
//! rewrite after common lowering; it never re-reads the AST or resolves a declaration from its name.

use crate::fx_hash::{HashMap, HashSet};

use crate::ir::{ClassId, ExprId, IrExpr, IrFile, IrFunction, IrStatic};
use crate::jvm::names::{property_getter_name, property_setter_name};
use crate::types::{Ty, Visibility};

#[derive(Clone)]
struct Candidate {
    outer: ClassId,
    companion: ClassId,
    property: usize,
    field: u32,
    /// The declaration's initializer and its store in the companion body. A property assigned
    /// only by `init` blocks has neither: those assignments become static stores in place.
    initializer: Option<(ExprId, ExprId)>,
    name: String,
    ty: Ty,
    is_var: bool,
    visibility: Visibility,
    /// A private property has no companion accessor. In-class reads become static reads of the
    /// hoisted field, and another class reaches it through `access$get…$cp`.
    is_private: bool,
    /// The visibility of a synthesized setter; a getter has the property's own.
    setter_visibility: Visibility,
    source_order: u32,
    decl_line: u32,
    accessor_lines: crate::ir::IrAccessorLines,
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
    /// A `lateinit` companion property is public static storage on the outer class. Its companion
    /// getter retains the uninitialized guard; the field itself carries no nullability annotation.
    is_lateinit: bool,
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

/// Whether a synthetic field that class emission adds after JVM storage realization reserves this
/// spelling. Ordinary private storage yields by name; a public source field may share the name at
/// a distinct descriptor. A displaced delegated-property array's exact collision is the exception:
/// it stays physical so declaration-collision validation can report the two source declarations.
fn synthetic_owner_static_field(
    ir: &IrFile,
    delegated_arrays: &crate::jvm::property_references::DelegatedPropertyArrays,
    owner: ClassId,
    name: &str,
    descriptor: &str,
    public_source_field: bool,
) -> bool {
    let class = &ir.classes[owner as usize];
    let companion_field = class.companion_class.is_some_and(|companion| {
        name == companion.nested_segment_ref()
            && (!public_source_field || descriptor == format!("L{};", companion.render()))
    });
    if companion_field {
        return true;
    }
    let array_name = delegated_arrays.field_name(class.fq_name);
    if name == array_name && delegated_arrays.elements(class.fq_name).is_some() {
        let exact_array_signature =
            descriptor == crate::jvm::property_references::delegated_arrays::DESCRIPTOR;
        if public_source_field {
            return exact_array_signature;
        }
        if array_name == crate::jvm::property_references::delegated_arrays::FIELD {
            return true;
        }
        // An ordinary field at a displaced `$N` spelling yields when its descriptor differs. An
        // exact signature stays in place so declaration-collision validation can reject the two
        // source declarations instead of silently suffixing the private storage.
        return !exact_array_signature;
    }
    if !class.is_enum {
        return false;
    }
    // Entry constants keep their ABI names, so ordinary private storage with the same source name
    // receives `$N` even when its descriptor differs. A public `@JvmField` is itself source ABI,
    // though, and the classfile admits it beside an entry when their full signatures differ.
    // `$VALUES` and `$ENTRIES` are different: kotlinc preserves a source property's spelling and
    // suffixes the generated enum storage instead (selected by `enum_synthetic_fields`).
    class.enum_entries.iter().any(|entry| entry.name == name)
        && (!public_source_field || descriptor == format!("L{};", class.fq_name.render()))
}

/// Realize the JVM's companion-property storage layout from semantic common IR.
pub(super) fn lower_companion_properties(
    ir: &mut IrFile,
    realizations: &crate::jvm::property_realizations::PropertyRealizations,
    physical_top_lambda_ordinals: &mut crate::jvm::lifted_names::PhysicalTopLambdaOrdinals,
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
        if outer_class.is_value {
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
            let backing = &companion_class.fields[field as usize];
            let visibility = declaration.visibility;
            let is_jvm_field = jvm_field_eligible(declaration, backing);
            let initializer = match declaration.initializer {
                Some(initializer) => match initializer_store(ir, companion, field, initializer) {
                    Some(store) => Some((initializer, store)),
                    None => continue,
                },
                None => None,
            };
            // Every visibility is hoisted: kotlinc stores a private, protected or internal
            // property as a private static of the outer class too, not as a companion instance
            // field. An interface companion cannot host that field, and this loop never selects
            // one.
            // A custom getter or setter does not keep the field on the companion. kotlinc
            // still stores it as a private static of the outer class, and the accessor body
            // reads that static through `access$…$cp`.
            // A value-class property is hoisted too. kotlinc stores it as a private static of
            // the outer class — the carrier when that carrier can represent the value, the box
            // when it cannot (`I?` over `Int`). The value-class pass records which of the two
            // this static is; leaving the field on the companion made a property reference call
            // an instance accessor the class does not have.
            if declaration.is_open {
                continue;
            }
            candidates.push(Candidate {
                outer,
                companion,
                property,
                field,
                initializer,
                name: declaration.name.clone(),
                ty: declaration.ty,
                is_var: declaration.is_var,
                visibility,
                is_private: declaration.is_private,
                setter_visibility: declaration.setter_visibility,
                source_order: declaration.source_order,
                decl_line: declaration.decl_line,
                accessor_lines: declaration.accessor_lines,
                is_jvm_field,
                declared_getter: declaration.getter,
                declared_setter: declaration.setter,
                delegate_storage: false,
                is_lateinit: backing.is_lateinit(),
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
                initializer: Some((initializer, initializer_store)),
                name: backing.name.clone(),
                ty: backing.ty,
                is_var: false,
                visibility: Visibility::Private,
                is_private: true,
                setter_visibility: Visibility::Private,
                source_order: declaration.source_order,
                decl_line: declaration.decl_line,
                accessor_lines: declaration.accessor_lines,
                is_jvm_field: false,
                declared_getter: declaration.getter,
                declared_setter: declaration.setter,
                delegate_storage: true,
                is_lateinit: false,
            });
        }
    }
    // Ordinary backing fields and delegate storage are discovered in separate shape-specific
    // passes above. Rejoin them at the recorded declaration boundary before assigning global
    // static identities: class and enum `<clinit>` consume those identities in insertion order.
    candidates.sort_by_key(|candidate| {
        (
            candidate.outer,
            candidate.source_order,
            candidate.property,
            candidate.delegate_storage,
        )
    });
    if candidates.is_empty() {
        schedule_class_companion_initializers(ir, realizations, physical_top_lambda_ordinals);
        return;
    }

    let mut static_for_field = HashMap::default();
    for candidate in &candidates {
        let index = ir.statics.len() as u32;
        let accessors = crate::ir::IrStaticAccessors {
            getter: if candidate.declared_getter.is_some() {
                crate::ir::IrStaticAccessor::Declared
            } else {
                crate::ir::IrStaticAccessor::Default
            },
            setter: if candidate.declared_setter.is_some() {
                crate::ir::IrStaticAccessor::Declared
            } else if !candidate.is_private && candidate.setter_visibility.is_private() {
                crate::ir::IrStaticAccessor::Absent
            } else {
                crate::ir::IrStaticAccessor::Default
            },
        };
        ir.statics.push(IrStatic {
            name: candidate.name.clone(),
            ty: candidate.ty,
            init: candidate.initializer.map(|(initializer, _)| initializer),
            is_var: candidate.is_var,
            is_const: false,
            is_lateinit: candidate.is_lateinit,
            owner: Some(ir.classes[candidate.outer as usize].fq_name),
            visibility: candidate.visibility,
            getter_jvm_name: None,
            setter_jvm_name: None,
            erased_declared_ty: None,
            accessors,
            line: candidate.decl_line,
            accessor_lines: crate::ir::IrAccessorLines::default(),
            source_order: candidate.source_order,
            // kotlinc's hoisted field is a new declaration, not the delegate storage it moved:
            // it keeps its generic `Signature`.
            property_delegate: false,
        });
        // Deliberately NOT registered in `declared_class_statics`: that table feeds the owner's
        // `@Metadata` const-property records, and kotlinc's owner metadata has NO record for a
        // hoisted companion property (the declaration belongs to the companion's metadata alone).
        // Emission finds the physical field through `IrStatic.owner`.
        ir.mark_jvm_companion_hoisted_static(index);
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
        .filter_map(|candidate| candidate.initializer.map(|(_, store)| store))
        .collect();
    let affected_companions: HashSet<_> = candidates
        .iter()
        .map(|candidate| candidate.companion)
        .collect();
    let mut retain_initializer_stores = HashSet::default();
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
        // Hoisted stores become outer-class static stores. When the body also has an `init` block
        // (or any other statement), keep every declaration store in that same body so its source
        // order remains indivisible. The scheduler below either moves the complete body to the
        // outer `<clinit>`, or leaves the complete body on the companion constructor when a field
        // that cannot be hoisted still needs the companion instance.
        if has_other_statement && class_companion_clinit_owner(ir, companion).is_some() {
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
            IrExpr::GetField {
                receiver,
                class,
                index,
            } if static_for_field.contains_key(&(class, index)) => {
                let read = IrExpr::GetStatic(static_for_field[&(class, index)]);
                // A canonical companion singleton read still triggers its class initialization
                // before the static access. An arbitrary expression written as the receiver is a
                // lookup coordinate for a companion property and is not evaluated by kotlinc.
                if redundant_companion_receiver(ir, class, receiver)
                    && !crate::ir::expr_runs_no_code(ir, receiver)
                {
                    let read = ir.add_expr(read);
                    Some(IrExpr::Block {
                        stmts: vec![receiver],
                        value: Some(read),
                    })
                } else {
                    Some(read)
                }
            }
            IrExpr::SetField {
                receiver: _,
                class,
                index,
                value,
            } if static_for_field.contains_key(&(class, index)) => {
                // The checked field identity is now outer static storage. Its companion receiver
                // was the declaration's lookup coordinate; the outer `putstatic` triggers the
                // same class initialization and kotlinc does not emit a `Companion` load/pop.
                Some(IrExpr::SetStatic {
                    index: static_for_field[&(class, index)],
                    value,
                })
            }
            IrExpr::LateinitInitialized {
                receiver,
                class,
                index,
            } if static_for_field.contains_key(&(class, index)) => {
                let read = IrExpr::GetStatic(static_for_field[&(class, index)]);
                // Keep the raw-read identity when the canonical singleton still has to trigger
                // companion initialization. The nested read, rather than its wrapping block, is
                // the expression emitted as getstatic and therefore owns the no-guard marker.
                if redundant_companion_receiver(ir, class, receiver)
                    && !crate::ir::expr_runs_no_code(ir, receiver)
                {
                    let read = ir.add_expr(read);
                    ir.lateinit_initialization_probes.insert(read);
                    Some(IrExpr::Block {
                        stmts: vec![receiver],
                        value: Some(read),
                    })
                } else {
                    Some(read)
                }
            }
            _ => None,
        };
        if let Some(replacement) = replacement {
            ir.exprs[expression] = replacement;
        }
    }

    // Compact each companion's field table and retarget all surviving field identities.
    let mut removed = HashMap::<ClassId, HashSet<u32>>::default();
    for &(companion, field) in static_for_field.keys() {
        removed.entry(companion).or_default().insert(field);
    }
    crate::jvm::field_compaction::remove_fields(ir, &removed);

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
                ir.set_method_visibility(getter, candidate.visibility);
                getter
            });
            let setter = (candidate.is_var
                && candidate.declared_setter.is_none()
                && !candidate.setter_visibility.is_private())
            .then(|| {
                let value = ir.add_expr(IrExpr::GetValue(1));
                let write = ir.add_expr(IrExpr::SetStatic {
                    index: static_index,
                    value,
                });
                let returned = ir.add_expr(IrExpr::Return(None));
                let setter_line = if candidate.accessor_lines.setter != 0 {
                    candidate.accessor_lines.setter
                } else {
                    candidate.decl_line
                };
                if setter_line != 0 {
                    ir.expr_lines.insert(write, setter_line);
                }
                // This default setter is backend-synthesized, but its source declaration is not:
                // a visibility-only `lateinit` accessor owns the prefix line at its store and the
                // whole property's final line at its explicit return. Record those identities at
                // the producer so ordinary expression emission places their exact physical PCs.
                if candidate.is_lateinit
                    && candidate.accessor_lines.setter != 0
                    && candidate.accessor_lines.declaration_end > setter_line
                {
                    ir.expr_lines
                        .insert(returned, candidate.accessor_lines.declaration_end);
                }
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
                ir.set_method_visibility(setter, candidate.setter_visibility);
                // kotlinc writes the setter's `<set-?>` local with the setter, before the next
                // member, so its table is recorded as the body is emitted.
                ir.fn_debug_locals.insert(setter);
                setter
            });
            (getter, setter)
        });

        // A property only `init` blocks assign has no static initializer: those stores were
        // rewritten in place above.
        // In a retained body the store stays, run by `<clinit>` with the instance already in
        // value 0; a separate static initializer would store the value again.
        let separate_initializer = candidate
            .initializer
            .filter(|_| !retain_initializer_stores.contains(&candidate.companion));
        ir.statics[static_index as usize].init = None;
        if let Some((mut initializer, _)) = separate_initializer {
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
            // The outer `<clinit>` now evaluates the initializer, so it owns the lambdas too.
            let outer = ir.classes[candidate.outer as usize].fq_name;
            move_initializer_lambdas(
                ir,
                candidate.companion,
                outer,
                initializer,
                physical_top_lambda_ordinals,
            );
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
    schedule_class_companion_initializers(ir, realizations, physical_top_lambda_ordinals);
}

/// Allocate the final JVM names of companion fields hoisted onto their enclosing classes.
///
/// This runs after value-class lowering because a field's descriptor participates in its JVM
/// identity. Two source-distinct properties can acquire the same descriptor only after one has
/// been replaced by its carrier. The enclosing class's own fields retain their names except for
/// private companion-block storage that collides with a public `@JvmField`; that public ABI keeps
/// its source name and the private storage receives kotlinc's next free `$N` suffix. Other colliding
/// hoisted fields receive the suffix instead. Accessors and bridges retain the source property name.
pub(crate) fn finalize_hoisted_static_field_names(
    ir: &mut IrFile,
    delegated_arrays: &crate::jvm::property_references::DelegatedPropertyArrays,
) {
    let jvm_field_signatures = ir
        .statics
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            ir.is_jvm_companion_hoisted_static(*index as u32)
                && ir.is_jvm_field_static(*index as u32)
        })
        .filter_map(|(index, field)| {
            Some((
                field.owner?,
                ir.static_field_jvm_name(index as u32).to_string(),
                crate::jvm::names::type_descriptor(field.ty),
            ))
        })
        .collect::<HashSet<_>>();
    let mut declared_signatures = ir
        .statics
        .iter()
        .enumerate()
        .filter_map(|(index, field)| {
            Some((
                field.owner?,
                ir.static_field_jvm_name(index as u32).to_string(),
                crate::jvm::names::type_descriptor(field.ty),
            ))
        })
        .collect::<HashSet<_>>();
    let displaced_block_storage = (0..ir.statics.len() as u32)
        .filter(|&index| {
            !ir.is_jvm_companion_hoisted_static(index)
                && ir.companion_blocks.is_storage(index)
                && !ir.statics[index as usize].is_const
        })
        .filter(|&index| {
            let field = &ir.statics[index as usize];
            field.owner.is_some_and(|owner| {
                jvm_field_signatures.contains(&(
                    owner,
                    ir.static_field_jvm_name(index).to_string(),
                    crate::jvm::names::type_descriptor(field.ty),
                ))
            })
        })
        .collect::<Vec<_>>();
    for index in displaced_block_storage {
        let field = &ir.statics[index as usize];
        let owner = field
            .owner
            .expect("companion-block static storage has its declaring class as owner");
        let class = ir
            .class_id_by_name(owner)
            .expect("companion-block static storage owner is declared in this file");
        let descriptor = crate::jvm::names::type_descriptor(field.ty);
        let physical = (1usize..)
            .map(|suffix| format!("{}${suffix}", field.name))
            .find(|candidate| {
                !synthetic_owner_static_field(
                    ir,
                    delegated_arrays,
                    class,
                    candidate,
                    &descriptor,
                    false,
                ) && !declared_signatures.contains(&(owner, candidate.clone(), descriptor.clone()))
            })
            .expect("an unused companion-block field suffix always exists");
        declared_signatures.insert((owner, physical.clone(), descriptor));
        ir.set_jvm_static_field_name(index, physical);
    }

    let mut occupied = HashSet::default();
    for (index, field) in ir.statics.iter().enumerate() {
        if ir.is_jvm_companion_hoisted_static(index as u32) {
            continue;
        }
        let Some(owner) = field.owner else {
            continue;
        };
        occupied.insert((
            owner,
            ir.static_field_jvm_name(index as u32).to_string(),
            crate::jvm::names::type_descriptor(field.ty),
        ));
    }

    let hoisted = (0..ir.statics.len() as u32)
        .filter(|&index| ir.is_jvm_companion_hoisted_static(index))
        .collect::<Vec<_>>();
    for index in hoisted {
        let field = &ir.statics[index as usize];
        let owner = field
            .owner
            .expect("a companion-hoisted static has an enclosing class owner");
        let class = ir
            .class_id_by_name(owner)
            .expect("a companion-hoisted static owner is declared in this file");
        let descriptor = crate::jvm::names::type_descriptor(field.ty);
        let current = ir.static_field_jvm_name(index).to_string();
        // Ordinary private storage yields to every generated owner-field spelling by name.
        // A public `@JvmField` owns source ABI and may coexist at a distinct descriptor.
        let public_source_field = ir.is_jvm_field_static(index);
        let is_occupied = |name: &str| {
            synthetic_owner_static_field(
                ir,
                delegated_arrays,
                class,
                name,
                &descriptor,
                public_source_field,
            ) || occupied.contains(&(owner, name.to_string(), descriptor.clone()))
        };
        let physical = if is_occupied(&current) {
            (1usize..)
                .map(|suffix| format!("{}${suffix}", field.name))
                .find(|candidate| !is_occupied(candidate))
                .expect("an unused JVM static field suffix always exists")
        } else {
            current
        };
        occupied.insert((owner, physical.clone(), descriptor));
        if physical != ir.static_field_jvm_name(index) {
            ir.set_jvm_static_field_name(index, physical);
        }
    }
}

/// A class companion's initializer runs in the enclosing class `<clinit>`, after that class has
/// stored the companion instance. Property initializers and `init` blocks stay in source order
/// inside the body. The companion constructor is left as `super()` only.
///
/// An enum class runs it after its entries, `$VALUES` and `$ENTRIES`, where it stores the
/// companion instance. Interface companions keep object storage and run their initializer in
/// their own `<clinit>`. A named object's companion stays with that object's initializer.
fn schedule_class_companion_initializers(
    ir: &mut IrFile,
    realizations: &crate::jvm::property_realizations::PropertyRealizations,
    physical_top_lambda_ordinals: &mut crate::jvm::lifted_names::PhysicalTopLambdaOrdinals,
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
            &HashSet::default(),
            &HashSet::default(),
        ) {
            continue;
        }
        scheduled.push((ir.classes[index].fq_name, companion, body));
    }
    for (owner, companion, body) in scheduled {
        // The initializer now emits in the outer `<clinit>`. A lambda it evaluates is a private
        // static of that class; leaving it on the companion makes the outer class call a private
        // method it cannot see.
        move_initializer_lambdas(ir, companion, owner, body, physical_top_lambda_ordinals);
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

/// Replace companion-`this` reads in `body` with a load of the outer `Companion` field.
fn reload_companion_instance(
    ir: &mut IrFile,
    outer: crate::types::TypeName,
    companion: ClassId,
    body: ExprId,
) {
    let companion_name = ir.classes[companion as usize].fq_name;
    let field = companion_name.nested_segment_ref().to_string();
    // A lambda's `inline_body` numbers its own values: its value 0 is not the companion.
    for expression in crate::ir::value_namespace_expressions(ir, body) {
        if matches!(ir.expr(expression), IrExpr::GetValue(0)) {
            ir.exprs[expression as usize] = IrExpr::ExternalStaticInstance {
                owner: outer,
                ty: companion_name,
                field: field.clone(),
            };
        }
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
    let mut seen = HashSet::default();
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

/// Move lambda implementation methods used by `body` from the companion onto `outer`.
///
/// Lowering attaches each lambda to the companion, the class whose constructor originally held
/// the initializer. After the body moves, the `invokedynamic` is emitted by the outer class.
fn move_initializer_lambdas(
    ir: &mut IrFile,
    companion: ClassId,
    outer: crate::types::TypeName,
    body: ExprId,
    physical_top_lambda_ordinals: &mut crate::jvm::lifted_names::PhysicalTopLambdaOrdinals,
) {
    let Some(outer_class) = ir.class_id_by_name(outer) else {
        return;
    };
    let mut pending = vec![body];
    let mut seen = HashSet::default();
    let mut reachable_lambdas = HashSet::default();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::Lambda { impl_fn, .. } = ir.expr(expression) {
            if reachable_lambdas.insert(*impl_fn) {
                if let Some(nested) = ir
                    .functions
                    .get(*impl_fn as usize)
                    .and_then(|function| function.body)
                {
                    pending.push(nested);
                }
            }
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    // The expression walk is deliberately stack based and therefore does not preserve sibling
    // order. The companion's method inventory does: retain that source order while also collapsing
    // repeated expression edges to one implementation.
    let mut selected = HashSet::default();
    let moving = ir.classes[companion as usize]
        .methods
        .iter()
        .copied()
        .filter(|function| reachable_lambdas.contains(function) && selected.insert(*function))
        .collect::<Vec<_>>();
    if moving.is_empty() {
        return;
    }
    ir.classes[companion as usize]
        .methods
        .retain(|method| !moving.contains(method));
    // Implementation names are numbered per class: a moved lambda continues the outer class's
    // sequence for its name, as kotlinc numbers it there. Only the outermost implementation owns a
    // class-level ordinal; a callable nested beneath it keeps the remainder of that root's lifted
    // path. Allocate each name-group in its original source ordinal order; the stack walk above is
    // intentionally irrelevant to that order.
    let mut renumber = moving
        .iter()
        .filter_map(|&function| {
            if !owns_class_lambda_ordinal(ir, function) {
                return None;
            }
            let origin = ir.lambda_origins.get(&function)?;
            Some((
                origin.implementation_name.clone(),
                origin.implementation_ordinal,
                origin.identity,
                function,
            ))
        })
        .collect::<Vec<_>>();
    renumber.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then(left.1.cmp(&right.1))
            .then(left.2.cmp(&right.2))
            .then(left.3.cmp(&right.3))
    });
    let mut next_ordinals = crate::fx_hash::HashMap::<String, u32>::default();
    for (name, _, _, function) in renumber {
        let ordinal = {
            let next = next_ordinals.entry(name.clone()).or_insert_with(|| {
                ir.classes[outer_class as usize]
                    .methods
                    .iter()
                    .filter_map(|method| {
                        owns_class_lambda_ordinal(ir, *method)
                            .then(|| ir.lambda_origins.get(method))
                            .flatten()
                    })
                    .filter(|other| other.implementation_name == name)
                    .map(|other| other.implementation_ordinal)
                    .max()
                    .map_or(0, |ordinal| {
                        ordinal
                            .checked_add(1)
                            .expect("lambda implementation ordinal overflow")
                    })
            });
            let ordinal = *next;
            *next = next
                .checked_add(1)
                .expect("lambda implementation ordinal overflow");
            ordinal
        };
        ir.lambda_origins
            .get_mut(&function)
            .expect("a selected lambda origin remains present")
            .implementation_ordinal = ordinal;
    }
    physical_top_lambda_ordinals.record_moved(ir, &moving);
    for &function in &moving {
        let methods = &mut ir.classes[outer_class as usize].methods;
        if !methods.contains(&function) {
            methods.push(function);
        }
    }
    let owner = ir.classes[outer_class as usize].fq_name_id();
    for function in moving {
        ir.class_static_local_functions.insert(function, owner);
    }
}

/// Whether one lambda implementation owns an ordinal in its physical class's `$lambda$N`
/// namespace. A structured multi-step lifting path proves that the implementation is nested below
/// another lambda and therefore shares that root's class-level ordinal. An implementation without
/// lifting provenance still owns the ordinal recorded in its [`crate::ir::IrLambdaOrigin`].
fn owns_class_lambda_ordinal(ir: &IrFile, function: crate::ir::FunId) -> bool {
    let Some((_, site)) = ir.lifted_functions.get(&function) else {
        return true;
    };
    site.lifted
        && matches!(
            site.path.as_ref(),
            [step]
                if step.kind == crate::lifting_provenance::LiftingCallableKind::Lambda
        )
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
    let mut seen = HashSet::default();
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
