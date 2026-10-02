//! JVM realization of checked local delegated-property access plans.
//!
//! The frontend/common pipeline records selected convention templates and source provenance only.
//! Non-lambda declarations have one private-static helper at their declaration owner; inline
//! copies retain a typed prototype and use its access boundary. A lambda's private helper and
//! reflection table are instead owned by each exact source or specialized closure realization.

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

#[derive(Clone, Copy)]
pub(crate) struct ForeignHelperOwner {
    pub(crate) classifier: TypeName,
    pub(crate) is_interface: bool,
}

/// Private-static access requirements selected while declaration provenance is still available.
/// These JVM-only facts travel beside common IR through representation and emission.
#[derive(Default)]
pub(crate) struct HelperAccess {
    exported: Vec<(u32, Option<TypeName>)>,
    foreign: HashMap<u32, ForeignHelperOwner>,
    inline_delegates: HashSet<u32>,
    lambda_paths: HashMap<u32, usize>,
    property_owners: HashMap<ExprId, TypeName>,
}

impl HelperAccess {
    pub(crate) fn exported(&self) -> impl Iterator<Item = (u32, Option<TypeName>)> + '_ {
        self.exported.iter().copied()
    }

    pub(crate) fn requires_accessor(&self, function: u32) -> bool {
        self.foreign.contains_key(&function)
            || self.exported.iter().any(|&(helper, _)| helper == function)
    }

    pub(crate) fn foreign_owner(&self, function: u32) -> Option<ForeignHelperOwner> {
        self.foreign.get(&function).copied()
    }

    pub(crate) fn uses_inline_delegate_name(&self, function: u32) -> bool {
        // A foreign prototype has the same declaration ABI even though it exports nothing here.
        self.inline_delegates.contains(&function)
    }

    pub(crate) fn lambda_path_start(&self, function: u32) -> Option<usize> {
        self.lambda_paths.get(&function).copied()
    }

    pub(crate) fn closure_property_owner(&self, expression: ExprId) -> Option<TypeName> {
        self.property_owners.get(&expression).copied()
    }
}

pub(crate) fn realize(
    ir: &mut IrFile,
    current_source: IrModuleSource,
    stems: &[String],
    classifiers: &crate::backend::CheckedBackendClassifiers<'_>,
) -> Result<HelperAccess, ()> {
    let closure_plans = super::local_delegate_closures::bind(ir)?;
    let plans = std::mem::take(&mut ir.local_delegate_plans);
    let live = emitted_expressions(ir);
    let mut accesses = live
        .iter()
        .filter_map(|&expression| match ir.expr(expression) {
            IrExpr::LocalDelegateAccess(access) => Some((expression, access.plan)),
            _ => None,
        })
        .collect::<Vec<_>>();
    accesses.sort_unstable_by_key(|&(expression, _)| expression);
    // Lift helpers in checked declaration-plan order so unrelated expression allocation does not
    // perturb kotlinc-compatible local/lambda numbering. A declaration owns its helper even when
    // its source function is an inline-only template or the local is unread.
    let mut uses = plans
        .iter()
        .enumerate()
        .filter(|(plan, declaration)| {
            !closure_plans.is_template(*plan as u32)
                && (declaration.reference.source == current_source
                    || closure_plans.lambda_path_start(*plan as u32).is_some())
        })
        .map(|(plan, declaration)| {
            Ok((
                u32::try_from(plan).map_err(|_| ())?,
                declaration.reference.member_order,
                declaration.reference.ordinal,
            ))
        })
        .collect::<Result<Vec<_>, ()>>()?;
    uses.sort_unstable_by_key(|&(plan, member_order, ordinal)| (member_order, ordinal, plan));
    let mut uses = uses.into_iter().map(|(key, _, _)| key).collect::<Vec<_>>();
    for &(_, plan) in &accesses {
        if closure_plans.is_template(plan) {
            return Err(());
        }
        if !uses.contains(&plan) {
            uses.push(plan);
        }
    }

    let mut helper_access = HelperAccess::default();
    let mut realizations = HashMap::new();
    for key in uses {
        let plan = plans.get(key as usize).ok_or(())?;
        let lambda_path = closure_plans.lambda_path_start(key);
        let owner = match lambda_path {
            Some(_) => Some(closure_plans.helper_owner(key).ok_or(())?),
            None => plan.reference.class,
        };
        let foreign = (lambda_path.is_none() && plan.reference.source != current_source)
            .then(|| {
                let classifier = match owner {
                    Some(owner) => owner,
                    None => crate::jvm::module_calls::facade_for(plan.reference.source, stems)
                        .ok_or(())?,
                };
                let is_interface = match owner {
                    Some(owner) => classifiers
                        .module()
                        .classifier(owner)
                        .ok_or(())?
                        .is_interface(),
                    None => false,
                };
                Ok::<_, ()>(ForeignHelperOwner {
                    classifier,
                    is_interface,
                })
            })
            .transpose()?;
        let inline_delegate = plan.inline_declaration.is_some();
        let exported = inline_delegate && lambda_path.is_none();
        let getter = realize_accessor(
            ir,
            &plan.storage_name,
            owner,
            plan.reference.source.source,
            plan.getter.clone(),
            foreign.is_none(),
        )?;
        let setter = plan
            .setter
            .as_ref()
            .map(|accessor| {
                realize_accessor(
                    ir,
                    &plan.storage_name,
                    owner,
                    plan.reference.source.source,
                    accessor.clone(),
                    foreign.is_none(),
                )
            })
            .transpose()?;
        for function in std::iter::once(getter).chain(setter) {
            if inline_delegate {
                helper_access.inline_delegates.insert(function);
            }
            if let Some(start) = lambda_path {
                helper_access.lambda_paths.insert(function, start);
            }
            if let Some(foreign) = foreign {
                helper_access.foreign.insert(function, foreign);
            } else if exported {
                helper_access.exported.push((function, owner));
            }
        }
        realizations.insert(
            key,
            Realization {
                getter,
                setter,
                owner,
            },
        );
    }

    helper_access.property_owners = closure_plans.into_property_owners();
    let live_accesses = accesses.into_iter().collect::<HashMap<_, _>>();
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
        let callee = if helper_access.foreign_owner(function).is_some() {
            // A typed prototype participates in ABI transforms but is emitted by its declaration
            // file. The private-static boundary consumes its exact physical owner at emission.
            Callee::Local(function)
        } else {
            realization
                .owner
                .map_or(Callee::Local(function), |owner| Callee::ClassStatic {
                    owner,
                    function,
                })
        };
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
    let live = emitted_expressions(ir);
    for raw in 0..ir.exprs.len() {
        if matches!(ir.exprs[raw], IrExpr::LocalPropertyReference(_))
            && !live.contains(&(raw as ExprId))
        {
            ir.exprs[raw] = IrExpr::UnitInstance;
        }
    }
    Ok(helper_access)
}

/// Expressions reachable from emitted code, independent of the caller's physical owner.
/// Inline-only templates are deliberately not roots; their call-site copies are.
fn emitted_expressions(ir: &IrFile) -> HashSet<ExprId> {
    let mut roots = Vec::new();
    for (function, declaration) in ir.functions.iter().enumerate() {
        let function = function as u32;
        if !ir.inline_only_fns.contains(&function) {
            roots.extend(declaration.body);
            if let Some(defaults) = ir
                .fn_params
                .get(&function)
                .and_then(|parameters| parameters.defaults.as_ref())
            {
                roots.extend(defaults.iter().flatten().copied());
            }
        }
    }
    for class in &ir.classes {
        roots.extend(class.init_body);
        roots.extend(
            class
                .super_arg_prelude
                .iter()
                .chain(&class.super_args)
                .copied(),
        );
        roots.extend(
            class
                .properties
                .iter()
                .filter_map(|property| property.initializer),
        );
        for constructor in &class.secondary_ctors {
            roots.extend(
                constructor
                    .body
                    .iter()
                    .chain(constructor.defaults.iter().flatten())
                    .chain(&constructor.delegate_prelude)
                    .chain(&constructor.delegate_args)
                    .copied(),
            );
        }
        for entry in &class.enum_entries {
            roots.extend(entry.argument_prelude.iter().chain(&entry.args).copied());
        }
    }
    roots.extend(ir.statics.iter().filter_map(|property| property.init));
    if let Some(body) = ir.checked_script_body {
        roots.push(body);
    }

    let mut visited = HashSet::new();
    for root in roots {
        let mut pending = vec![root];
        while let Some(expression) = pending.pop() {
            if !visited.insert(expression) {
                continue;
            }
            match ir.expr(expression) {
                IrExpr::Lambda { captures, .. } => pending.extend(captures.iter().copied()),
                _ => crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
                    pending.push(child)
                }),
            }
        }
    }
    visited
}

fn realize_accessor(
    ir: &mut IrFile,
    source_name: &str,
    owner: Option<TypeName>,
    source: crate::fir::SourceFileId,
    accessor: crate::ir::IrLocalDelegateAccessorPlan,
    emit: bool,
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
        body: emit.then_some(returned),
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
    if !emit {
        ir.inline_only_fns.insert(function);
    } else if let Some(owner) = owner {
        let class = ir.class_id_by_name(owner).ok_or(())?;
        ir.classes[class as usize].methods.push(function);
        ir.class_static_local_functions.insert(function, owner);
    }
    Ok(function)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{type_name, Ty};

    #[test]
    fn a_shared_delegate_access_is_reachable_from_facade_and_class_roots() {
        let mut ir = IrFile::default();
        let delegate = ir.add_expr(IrExpr::UnitInstance);
        let access = ir.add_expr(IrExpr::LocalDelegateAccess(
            crate::ir::IrLocalDelegateAccess {
                plan: 0,
                delegate,
                dispatch_receiver: None,
                value: None,
            },
        ));
        ir.add_fun(IrFunction {
            name: "read".to_owned(),
            params: Vec::new(),
            param_checks: Vec::new(),
            ret: Ty::Unit,
            body: Some(access),
            is_static: true,
            dispatch_receiver: None,
        });
        let mut class = crate::ir::IrClass::synthetic(type_name("ReaderHost"));
        class.init_body = Some(access);
        ir.classes.push(class);

        assert_eq!(emitted_expressions(&ir), HashSet::from([delegate, access]));
    }

    #[test]
    fn an_inline_only_template_is_not_an_emitted_root() {
        let mut ir = IrFile::default();
        let body = ir.add_expr(IrExpr::UnitInstance);
        let function = ir.add_fun(IrFunction {
            name: "template".to_owned(),
            params: Vec::new(),
            param_checks: Vec::new(),
            ret: Ty::Unit,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
        });
        ir.inline_only_fns.insert(function);

        assert_eq!(emitted_expressions(&ir), HashSet::new());
    }
}
