//! Private delegate declarations owned by realized source and specialized closure classes.

use std::collections::{HashMap, HashSet};

use crate::ir::{FunId, IrExpr, IrFile, IrLocalDelegatePlan, IrModuleSource};
use crate::types::{Ty, TypeName};

/// The checked source lambdas whose inline declaration exposes a retained closure with delegates.
pub(super) struct Requirements {
    sources: HashMap<FunId, (IrModuleSource, bool)>,
}

impl Requirements {
    pub(super) fn collect(ir: &IrFile) -> Self {
        let sources = ir
            .local_delegate_plans
            .iter()
            .filter(|plan| plan.inline_declaration.is_some())
            .filter_map(|plan| {
                plan.declaration_lambda.map(|lambda| {
                    (
                        lambda,
                        (
                            plan.reference.source,
                            plan.inline_declaration
                                .expect("selected inline declaration")
                                .visibility
                                .is_public(),
                        ),
                    )
                })
            })
            .collect();
        Self { sources }
    }

    pub(super) fn source(&self, ir: &IrFile, function: FunId) -> Option<IrModuleSource> {
        self.sources
            .get(&source_function(ir, function))
            .map(|(source, _)| *source)
    }

    pub(super) fn public_inline(&self, ir: &IrFile, function: FunId) -> bool {
        self.sources
            .get(&source_function(ir, function))
            .is_some_and(|(_, public)| *public)
    }
}

fn source_function(ir: &IrFile, mut function: FunId) -> FunId {
    let mut seen = HashSet::new();
    while let Some(copy) = ir.specialized_functions.get(&function) {
        assert!(
            seen.insert(function),
            "a specialized callable has acyclic source provenance"
        );
        function = copy.source;
    }
    function
}

/// Regenerated closures keep a declaration's type-variable return slot. Specialization changes
/// its generic `Signature` and body, not that slot into a concrete return plus an invented bridge.
pub(super) fn invoke_result(ir: &IrFile, function: FunId) -> Option<Ty> {
    let declared = ir.functions[source_function(ir, function) as usize].ret;
    matches!(declared.non_null(), Ty::TyParam(..)).then_some(declared)
}

/// A closure's plans are backend copies of one checked declaration, not declaration-owned exports.
#[derive(Default)]
pub(super) struct BoundPlans {
    templates: HashSet<u32>,
    owned: HashMap<u32, (TypeName, usize)>,
    property_owners: HashMap<crate::ir::ExprId, TypeName>,
}

impl BoundPlans {
    pub(super) fn is_template(&self, plan: u32) -> bool {
        self.templates.contains(&plan)
    }

    pub(super) fn lambda_path_start(&self, plan: u32) -> Option<usize> {
        self.owned.get(&plan).map(|(_, start)| *start)
    }

    pub(super) fn helper_owner(&self, plan: u32) -> Option<TypeName> {
        self.owned.get(&plan).map(|(owner, _)| *owner)
    }

    pub(super) fn into_property_owners(self) -> HashMap<crate::ir::ExprId, TypeName> {
        self.property_owners
    }
}

/// Bind private helper templates to exact realized `invoke` identities. There is no placement
/// search: only a realized closure whose checked source lambda declares the plan may copy it.
pub(super) fn bind(ir: &mut IrFile) -> Result<BoundPlans, ()> {
    let templates = ir
        .local_delegate_plans
        .iter()
        .enumerate()
        .filter(|(_, plan)| plan.inline_declaration.is_some() && plan.declaration_lambda.is_some())
        .map(|(plan, declaration)| (plan as u32, declaration.clone()))
        .collect::<Vec<_>>();
    let closures = ir
        .classes
        .iter()
        .filter_map(|class| {
            let function = class.lambda.as_ref()?.invoke;
            Some((function, source_function(ir, function), class.fq_name_id()))
        })
        .collect::<Vec<_>>();
    let mut bound = BoundPlans {
        templates: templates.iter().map(|(plan, _)| *plan).collect(),
        owned: HashMap::new(),
        property_owners: HashMap::new(),
    };
    for (function, source, owner) in closures {
        let mut replacements = HashMap::new();
        for (template, declaration) in &templates {
            if declaration.declaration_lambda != Some(source) {
                continue;
            }
            let lambda_step = declaration
                .getter
                .site
                .path
                .iter()
                .rposition(|step| {
                    step.kind == crate::lifting_provenance::LiftingCallableKind::Lambda
                })
                .ok_or(())?;
            let mut plan = declaration.clone();
            clone_accessor(ir, &mut plan, false, owner, &mut bound.property_owners);
            if plan.setter.is_some() {
                clone_accessor(ir, &mut plan, true, owner, &mut bound.property_owners);
            }
            let copy = u32::try_from(ir.local_delegate_plans.len()).map_err(|_| ())?;
            ir.local_delegate_plans.push(plan);
            replacements.insert(*template, copy);
            bound.owned.insert(copy, (owner, lambda_step + 1));
        }
        let body = ir
            .functions
            .get(function as usize)
            .and_then(|function| function.body)
            .ok_or(())?;
        for expression in crate::ir::value_namespace_expressions(ir, body) {
            let IrExpr::LocalDelegateAccess(access) = &mut ir.exprs[expression as usize] else {
                continue;
            };
            if let Some(&plan) = replacements.get(&access.plan) {
                access.plan = plan;
            }
        }
    }
    Ok(bound)
}

fn clone_accessor(
    ir: &mut IrFile,
    plan: &mut IrLocalDelegatePlan,
    setter: bool,
    owner: TypeName,
    property_owners: &mut HashMap<crate::ir::ExprId, TypeName>,
) {
    let accessor = if setter {
        plan.setter
            .as_mut()
            .expect("a setter clone has a setter template")
    } else {
        &mut plan.getter
    };
    let (body, copies) = crate::ir::clone_expression_dag(ir, accessor.body);
    accessor.body = body;
    for expression in copies.values().copied() {
        ir.expression_owners.insert(expression, owner);
        if matches!(
            ir.exprs[expression as usize],
            IrExpr::LocalPropertyReference(_)
        ) {
            // Table storage follows the helper; the reference still names the checked lexical
            // declaration in PropertyReference0Impl's Class operand and metadata inventory.
            property_owners.insert(expression, owner);
        }
    }
}
