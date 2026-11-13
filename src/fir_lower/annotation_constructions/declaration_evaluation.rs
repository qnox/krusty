//! Canonical evaluation of closed source annotation declaration defaults.
//!
//! The checked argument vector already has declaration order. Remove its linear evaluation-order
//! holders here, retaining the real supplied expressions and their source facts. A runtime default
//! mask remains a runtime default mask: eagerly inserting omitted values changes enum initialization.

use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{ExprId, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Rewrite {
    Block(ExprId),
    AnnotationOperands(Vec<ExprId>),
    ArrayElements(Vec<ExprId>),
    Coercion(ExprId),
}

struct Binding {
    index: u32,
    value: ExprId,
    uses: usize,
}

#[derive(Default)]
struct Plan {
    edits: HashMap<ExprId, Rewrite>,
    bindings: Vec<Binding>,
    visiting: HashSet<ExprId>,
}

impl Plan {
    fn build(ir: &IrFile, root: ExprId, ty: Ty) -> Option<Self> {
        let mut plan = Self::default();
        let normalized = plan.expression(ir, root, ty)?;
        // Declaration roots retain their identity; only their original operand edges change.
        if normalized != root || super::element_value(ir, root, ty).is_none() {
            return None;
        }
        Some(plan)
    }

    fn record(&mut self, expression: ExprId, rewrite: Rewrite) -> Option<()> {
        if let Some(previous) = self.edits.get(&expression) {
            return (previous == &rewrite).then_some(());
        }
        self.edits.insert(expression, rewrite);
        Some(())
    }

    fn expression(&mut self, ir: &IrFile, expression: ExprId, ty: Ty) -> Option<ExprId> {
        if !self.visiting.insert(expression) {
            return None;
        }
        let value = match ir.expr(expression) {
            IrExpr::Const(_) | IrExpr::EnumEntry { .. } => expression,
            IrExpr::KClassLiteral {
                classifier: Some(_),
                value: None,
                type_argument: false,
            } => expression,
            IrExpr::GetValue(index) => {
                let binding = self.bindings.iter_mut().rev().find(|b| b.index == *index)?;
                binding.uses += 1;
                if binding.uses != 1 {
                    return None;
                }
                binding.value
            }
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                ..
            } => {
                let value = self.expression(ir, *arg, ty)?;
                if value != *arg {
                    self.record(expression, Rewrite::Coercion(value))?;
                }
                expression
            }
            IrExpr::Block {
                stmts,
                value: Some(result),
            } => {
                let base = self.bindings.len();
                for &statement in stmts {
                    let IrExpr::Variable {
                        index,
                        ty,
                        init: Some(initializer),
                        named: false,
                    } = ir.expr(statement)
                    else {
                        return None;
                    };
                    if !ir.argument_evaluation_bindings.contains(&statement) {
                        return None;
                    }
                    let value = self.expression(ir, *initializer, *ty)?;
                    self.bindings.push(Binding {
                        index: *index,
                        value,
                        uses: 0,
                    });
                }
                let value = self.expression(ir, *result, ty)?;
                if self.bindings[base..]
                    .iter()
                    .any(|binding| binding.uses != 1)
                {
                    return None;
                }
                self.bindings.truncate(base);
                if !stmts.is_empty() || value != *result {
                    self.record(expression, Rewrite::Block(value))?;
                }
                expression
            }
            IrExpr::Vararg {
                array_type,
                elements,
                spreads,
            } => {
                let element_ty = ty.non_null().array_read_elem()?;
                let mut values = Vec::with_capacity(elements.len());
                for (index, &element) in elements.iter().enumerate() {
                    let ty = if spreads.get(index).copied().unwrap_or(false) {
                        *array_type
                    } else {
                        element_ty
                    };
                    values.push(self.expression(ir, element, ty)?);
                }
                if &values != elements {
                    self.record(expression, Rewrite::ArrayElements(values))?;
                }
                expression
            }
            IrExpr::New {
                args,
                default_prefix_count: 0,
                ..
            } => {
                let mut values = Vec::with_capacity(args.len());
                for member in super::supplied_members(ir, expression)? {
                    values.push(self.expression(ir, member.value, member.ty)?);
                }
                if &values != args {
                    self.record(expression, Rewrite::AnnotationOperands(values))?;
                }
                expression
            }
            _ => return None,
        };
        self.visiting.remove(&expression);
        Some(value)
    }

    fn apply(self, ir: &mut IrFile) {
        for (expression, rewrite) in self.edits {
            match (rewrite, &mut ir.exprs[expression as usize]) {
                (Rewrite::Block(result), IrExpr::Block { stmts, value }) => {
                    stmts.clear();
                    *value = Some(result);
                }
                (Rewrite::AnnotationOperands(values), IrExpr::New { args, .. }) => *args = values,
                (Rewrite::ArrayElements(values), IrExpr::Vararg { elements, .. }) => {
                    *elements = values
                }
                (Rewrite::Coercion(value), IrExpr::TypeOp { arg, .. }) => *arg = value,
                _ => unreachable!("a validated default rewrite retains its node kind"),
            }
        }
    }
}

fn reachable(ir: &IrFile, mut roots: Vec<ExprId>) -> HashSet<ExprId> {
    let mut seen = HashSet::default();
    while let Some(expression) = roots.pop() {
        if seen.insert(expression) {
            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| roots.push(child));
        }
    }
    seen
}

/// Include retained templates and not-yet-materialized declarations. The emitted-body inventory
/// deliberately excludes those and therefore cannot alone prove a default's rewrite ownership.
fn ordinary_roots(ir: &IrFile) -> Vec<ExprId> {
    let mut roots = crate::ir::emitted_body_roots(ir);
    for (ordinal, function) in ir.functions.iter().enumerate() {
        roots.extend(function.body);
        let ordinal = u32::try_from(ordinal).expect("too many common IR functions");
        if let Some(defaults) = ir.param_defaults(ordinal) {
            roots.extend(defaults.iter().flatten().copied());
        }
    }
    for class in &ir.classes {
        if !class.is_annotation {
            if let Some(defaults) = ir.class_ctor_defaults_name(class.fq_name_id()) {
                roots.extend(defaults.iter().flatten().copied());
            }
        }
    }
    for constructor in ir.checked_constructor_bodies.values() {
        roots.extend(constructor.body);
        roots.extend(constructor.delegation);
        if !ir.classes[constructor.class as usize].is_annotation {
            roots.extend(constructor.defaults.iter().flatten().copied());
        }
    }
    for property in ir.checked_properties.values() {
        roots.extend(property.initializer);
        roots.extend(property.delegate);
        roots.extend(property.getter);
        roots.extend(property.setter);
    }
    roots.extend(ir.checked_class_initializers.iter().map(|value| value.body));
    roots.extend(
        ir.checked_enum_entry_bodies
            .values()
            .map(|value| value.construction),
    );
    for copy in ir.specialized_anonymous_classes.values() {
        roots.extend(copy.pending_property_roots.iter().copied());
    }
    for plan in &ir.local_delegate_plans {
        roots.push(plan.getter.body);
        roots.extend(plan.setter.as_ref().map(|setter| setter.body));
    }
    roots
}

pub(super) fn normalize(ir: &mut IrFile) {
    let mut roots = Vec::new();
    let mut protected_roots = ordinary_roots(ir);
    for class in ir.classes.iter().filter(|class| class.is_annotation) {
        if let Some(defaults) = ir.class_ctor_defaults_name(class.fq_name_id()) {
            if defaults.len() != class.ctor_args.len() {
                protected_roots.extend(defaults.iter().flatten().copied());
                continue;
            }
            roots.extend(
                class
                    .ctor_args
                    .iter()
                    .zip(defaults)
                    .filter_map(|(parameter, value)| Some(((*value)?, parameter.ty))),
            );
        }
    }
    normalize_roots(ir, roots, protected_roots);
}

fn normalize_roots(ir: &mut IrFile, roots: Vec<(ExprId, Ty)>, protected_roots: Vec<ExprId>) {
    let mut protected = reachable(ir, protected_roots);
    // Plan before mutating any root. Shared read-only children are fine; an edited parent shared
    // with an ordinary body, or with inconsistent scoped bindings, is left intact.
    let mut plans = Vec::new();
    let mut rewrites = HashMap::default();
    for (root, ty) in roots {
        let Some(plan) = Plan::build(ir, root, ty) else {
            protected.extend(reachable(ir, vec![root]));
            continue;
        };
        for (&expression, rewrite) in &plan.edits {
            if rewrites
                .insert(expression, rewrite.clone())
                .is_some_and(|old| old != *rewrite)
            {
                protected.insert(expression);
            }
        }
        plans.push((root, plan));
    }
    // Excluding a root protects all of its live descendants, including parents an otherwise
    // eligible default also shares. Propagate that ownership boundary before applying any edit.
    loop {
        let before = plans.len();
        plans.retain(|(root, plan)| {
            if plan
                .edits
                .keys()
                .any(|expression| protected.contains(expression))
            {
                protected.extend(reachable(ir, vec![*root]));
                false
            } else {
                true
            }
        });
        if plans.len() == before {
            break;
        }
    }
    for (_, plan) in plans {
        plan.apply(ir);
    }
}

#[cfg(test)]
mod tests;
