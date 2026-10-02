//! JVM realization of checked local delegated-property access plans.
//!
//! The frontend/common pipeline records selected convention templates and source provenance only.
//! The JVM chooses kotlinc's lifted private-static helper representation here. An inline template
//! can be copied into a different classifier, so helper placement follows the emitted function
//! containing each copy rather than the template's lexical classifier.

use std::collections::{HashMap, HashSet};

use crate::ir::{
    Callee, ExprId, FnParamInfo, IrExpr, IrFile, IrFunction, IrLiftingSequence, IrModuleSource,
};
use crate::types::{TypeName, Visibility};

#[derive(Clone, Copy)]
struct Realization {
    getter: u32,
    setter: Option<u32>,
    owner: Option<TypeName>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct PlanUse {
    plan: u32,
    owner: Option<TypeName>,
}

pub(crate) fn realize(ir: &mut IrFile, current_source: IrModuleSource) -> Result<(), ()> {
    let plans = std::mem::take(&mut ir.local_delegate_plans);
    let live = emitted_expression_owners(ir)?;

    // A copied inline body belongs to the caller for JVM reflection/storage. Rehome the copied
    // reference identity before the property-reference pass chooses its physical array.
    for (&expression, &owner) in &live {
        if ir.is_inline_copy(expression) {
            if let IrExpr::LocalPropertyReference(reference) = &mut ir.exprs[expression as usize] {
                reference.class = owner;
                reference.source = current_source;
            }
        }
    }

    let mut accesses = live
        .iter()
        .filter_map(|(&expression, &physical_owner)| match ir.expr(expression) {
            IrExpr::LocalDelegateAccess(access) => {
                let plan = plans.get(access.plan as usize)?;
                // A retained plan from another source can only be live through a call-site copy:
                // its template function is inline-only and therefore is not an emitted root. A
                // materialized lambda inside that template may replace the copied expression's
                // immediate inline mark while keeping the foreign declaration plan, so source
                // provenance is the stable proof that its helper belongs to the emitted owner.
                let owner = if ir.is_inline_copy(expression)
                    || ir.inline_local_delegate_plan_copies.contains(&access.plan)
                    || plan.reference.source != current_source
                {
                    physical_owner
                } else {
                    plan.reference.class
                };
                Some((expression, access.plan, owner))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    accesses.sort_unstable_by_key(|&(expression, _, _)| expression);
    let live_uses = accesses
        .iter()
        .map(|&(_, plan, owner)| PlanUse { plan, owner })
        .collect::<HashSet<_>>();

    // Lift helpers in checked declaration-plan order so unrelated expression allocation does not
    // perturb kotlinc-compatible local/lambda numbering. Within a plan, retain the first emitted
    // owner order (important when an inline template has copies in more than one classifier).
    let mut uses = plans
        .iter()
        .enumerate()
        .filter(|(plan, declaration)| {
            let Ok(plan) = u32::try_from(*plan) else {
                return false;
            };
            declaration.reference.source == current_source
                && !ir.inline_local_delegate_plan_copies.contains(&plan)
                && live_uses.contains(&PlanUse {
                    plan,
                    owner: declaration.reference.class,
                })
        })
        .map(|(plan, declaration)| {
            Ok((
                PlanUse {
                    plan: u32::try_from(plan).map_err(|_| ())?,
                    owner: declaration.reference.class,
                },
                declaration.reference.member_order,
                declaration.reference.ordinal,
            ))
        })
        .collect::<Result<Vec<_>, ()>>()?;
    uses.sort_unstable_by_key(|&(key, member_order, ordinal)| (member_order, ordinal, key.plan));
    let mut uses = uses.into_iter().map(|(key, _, _)| key).collect::<Vec<_>>();
    for &(_, plan, owner) in &accesses {
        let key = PlanUse { plan, owner };
        if !uses.contains(&key) {
            uses.push(key);
        }
    }

    let mut realizations = HashMap::new();
    for key in uses {
        let plan = plans.get(key.plan as usize).ok_or(())?;
        let owner = key.owner;
        let getter_plan = rehome_accessor(ir, &plan.getter, owner, current_source);
        let getter = realize_accessor(
            ir,
            &plan.storage_name,
            owner,
            plan.reference.source.source,
            getter_plan,
        )?;
        let setter = plan
            .setter
            .as_ref()
            .map(|accessor| {
                let accessor = rehome_accessor(ir, accessor, owner, current_source);
                realize_accessor(
                    ir,
                    &plan.storage_name,
                    owner,
                    plan.reference.source.source,
                    accessor,
                )
            })
            .transpose()?;
        realizations.insert(
            key,
            Realization {
                getter,
                setter,
                owner,
            },
        );
    }

    let live_accesses = accesses
        .into_iter()
        .map(|(expression, plan, owner)| (expression, PlanUse { plan, owner }))
        .collect::<HashMap<_, _>>();
    for raw in 0..ir.exprs.len() {
        let IrExpr::LocalDelegateAccess(access) = ir.exprs[raw].clone() else {
            continue;
        };
        let Some(key) = live_accesses.get(&(raw as ExprId)) else {
            // Foreign inline templates are common-IR sources, not emitted functions. Their copied
            // accesses were realized above; the template node itself is dead in this JVM file.
            ir.exprs[raw] = IrExpr::UnitInstance;
            continue;
        };
        let realization = realizations.get(key).ok_or(())?;
        let function = if access.value.is_some() {
            realization.setter.ok_or(())?
        } else {
            realization.getter
        };
        let callee =
            realization
                .owner
                .map_or(Callee::Local(function), |owner| Callee::ClassStatic {
                    owner,
                    function,
                });
        // A member-extension convention receives the enclosing instance before the delegate.
        let mut args = Vec::new();
        args.extend(access.dispatch_receiver);
        args.push(access.delegate);
        args.extend(access.value);
        ir.exprs[raw] = IrExpr::Call {
            callee,
            dispatch_receiver: None,
            args,
        };
    }

    // Property-reference realization scans the arena. Do not let dead retained inline templates
    // publish metadata or arrays; only references reachable from emitted code survive.
    let live = emitted_expression_owners(ir)?;
    for raw in 0..ir.exprs.len() {
        if matches!(ir.exprs[raw], IrExpr::LocalPropertyReference(_))
            && !live.contains_key(&(raw as ExprId))
        {
            ir.exprs[raw] = IrExpr::UnitInstance;
        }
    }
    Ok(())
}

/// The physical classifier containing every relevant expression reachable from emitted code.
/// Inline-only templates are deliberately not roots; their call-site copies are.
fn emitted_expression_owners(ir: &IrFile) -> Result<HashMap<ExprId, Option<TypeName>>, ()> {
    let mut class_functions = HashMap::new();
    for class in &ir.classes {
        for &function in &class.methods {
            class_functions.insert(function, class.fq_name_id());
        }
    }

    let mut roots = Vec::new();
    for (function, declaration) in ir.functions.iter().enumerate() {
        let function = function as u32;
        if !ir.inline_only_fns.contains(&function) {
            let owner = class_functions.get(&function).copied();
            roots.extend(declaration.body.map(|body| (body, owner)));
            if let Some(defaults) = ir
                .fn_params
                .get(&function)
                .and_then(|parameters| parameters.defaults.as_ref())
            {
                roots.extend(defaults.iter().flatten().map(|&body| (body, owner)));
            }
        }
    }
    for class in &ir.classes {
        let owner = Some(class.fq_name_id());
        roots.extend(class.init_body.map(|body| (body, owner)));
        roots.extend(
            class
                .super_arg_prelude
                .iter()
                .chain(&class.super_args)
                .copied()
                .map(|body| (body, owner)),
        );
        roots.extend(
            class
                .properties
                .iter()
                .filter_map(|property| property.initializer)
                .map(|body| (body, owner)),
        );
        for constructor in &class.secondary_ctors {
            roots.extend(
                constructor
                    .body
                    .iter()
                    .chain(constructor.defaults.iter().flatten())
                    .chain(&constructor.delegate_prelude)
                    .chain(&constructor.delegate_args)
                    .copied()
                    .map(|body| (body, owner)),
            );
        }
        for entry in &class.enum_entries {
            roots.extend(
                entry
                    .argument_prelude
                    .iter()
                    .chain(&entry.args)
                    .copied()
                    .map(|body| (body, owner)),
            );
        }
        roots.extend(
            ir.statics
                .iter()
                .filter(|property| property.owner == owner)
                .filter_map(|property| property.init)
                .map(|body| (body, owner)),
        );
    }
    roots.extend(
        ir.statics
            .iter()
            .filter(|property| property.owner.is_none())
            .filter_map(|property| property.init)
            .map(|body| (body, None)),
    );
    if let Some(body) = ir.checked_script_body {
        roots.push((body, None));
    }

    let mut relevant = HashMap::new();
    let mut visited = HashSet::new();
    for (root, owner) in roots {
        let mut pending = vec![root];
        while let Some(expression) = pending.pop() {
            if !visited.insert((expression, owner)) {
                continue;
            }
            if matches!(
                ir.expr(expression),
                IrExpr::LocalDelegateAccess(_) | IrExpr::LocalPropertyReference(_)
            ) && relevant
                .insert(expression, owner)
                .is_some_and(|seen| seen != owner)
            {
                return Err(());
            }
            match ir.expr(expression) {
                IrExpr::Lambda { captures, .. } => pending.extend(captures.iter().copied()),
                _ => crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
                    pending.push(child)
                }),
            }
        }
    }
    Ok(relevant)
}

fn rehome_accessor(
    ir: &mut IrFile,
    accessor: &crate::ir::IrLocalDelegateAccessorPlan,
    owner: Option<TypeName>,
    current_source: IrModuleSource,
) -> crate::ir::IrLocalDelegateAccessorPlan {
    let mut accessor = accessor.clone();
    let (body, copies) = crate::ir::clone_expression_dag(ir, accessor.body);
    accessor.body = body;
    for &copy in copies.values() {
        if let IrExpr::LocalPropertyReference(reference) = &mut ir.exprs[copy as usize] {
            reference.class = owner;
            reference.source = current_source;
        }
    }
    accessor
}

fn realize_accessor(
    ir: &mut IrFile,
    source_name: &str,
    owner: Option<TypeName>,
    source: crate::fir::SourceFileId,
    accessor: crate::ir::IrLocalDelegateAccessorPlan,
) -> Result<u32, ()> {
    if accessor.parameters.len() != accessor.parameter_identities.len() {
        return Err(());
    }
    if accessor.line != 0 {
        ir.expr_lines.insert(accessor.body, accessor.line);
        ir.expr_source_lines.insert(accessor.body, accessor.line);
    }
    let returned = if accessor.result == crate::types::Ty::Unit {
        let returned = ir.add_expr(IrExpr::Return(None));
        if accessor.line != 0 {
            ir.expr_source_lines.insert(returned, accessor.line);
        }
        ir.add_expr(IrExpr::Block {
            stmts: vec![accessor.body, returned],
            value: None,
        })
    } else {
        let returned = ir.add_expr(IrExpr::Return(Some(accessor.body)));
        if accessor.line != 0 {
            ir.expr_source_lines.insert(returned, accessor.line);
        }
        ir.add_expr(IrExpr::Block {
            stmts: vec![returned],
            value: None,
        })
    };
    let function = ir.add_fun(IrFunction {
        name: format!("{source_name}$jvm_delegate"),
        param_checks: vec![None; accessor.parameters.len()],
        params: accessor.parameters,
        ret: accessor.result,
        body: Some(returned),
        is_static: true,
        dispatch_receiver: None,
    });
    if !accessor.type_parameters.is_empty() {
        let signature = &ir.functions[function as usize];
        ir.signatures.insert(
            function,
            crate::ir::IrGenericSig {
                type_params: accessor.type_parameters,
                params: signature.params.clone(),
                ret: Some(signature.ret),
                supers: Vec::new(),
            },
        );
    }
    ir.fn_source_names.insert(function, source_name.to_owned());
    let mut parameters = FnParamInfo::identities(accessor.parameter_identities);
    parameters.captured_receivers = accessor.captured_receivers;
    ir.fn_params.insert(function, parameters);
    ir.set_method_visibility(function, Visibility::Private);
    if accessor.line != 0 {
        ir.fn_decl_lines.insert(function, accessor.line);
    }
    let sequence = IrLiftingSequence {
        source,
        owner: accessor.site.owner.clone(),
        container: accessor.site.container.clone(),
    };
    ir.lifting_sequence_source_order
        .entry(sequence.clone())
        .and_modify(|earliest| *earliest = (*earliest).min(accessor.source_order))
        .or_insert(accessor.source_order);
    ir.lifted_functions
        .insert(function, (sequence, accessor.site));
    if let Some(owner) = owner {
        let class = ir.class_id_by_name(owner).ok_or(())?;
        ir.classes[class as usize].methods.push(function);
        ir.class_static_local_functions.insert(function, owner);
    }
    Ok(function)
}
