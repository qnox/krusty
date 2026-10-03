//! Whether a lowered body executes a reified binding.
//!
//! A lambda implementation and an anonymous class ask this once. A direct type operation, a
//! recorded reified call, and a local-delegate plan getter or setter are executions. Nested
//! lambda implementations belong to the body that contains them. An anonymous class also follows
//! the classes it constructs; a lambda does not, because constructing another class does not
//! execute that class's members.

use std::collections::{HashMap, HashSet};

use crate::ir::{
    Callee, ClassId, ExprId, IrCheckedOperation, IrCheckedSubstitution, IrExpr, IrIntrinsic,
};
use crate::types::{ty_subst_keep_unbound, Ty};

pub(super) fn function_executes_binding(
    ir: &crate::ir::IrFile,
    function: u32,
    bindings: &HashMap<String, Ty>,
    decisions: &mut HashMap<u32, bool>,
) -> bool {
    if bindings.is_empty() {
        return false;
    }
    BindingWalk {
        ir,
        bindings,
        plans: HashSet::new(),
        functions: HashSet::new(),
        decisions,
        classes: HashSet::new(),
        follow_classes: false,
    }
    .function(function)
}

pub(super) fn root_executes_binding(
    ir: &crate::ir::IrFile,
    root: ExprId,
    bindings: &HashMap<String, Ty>,
) -> bool {
    if bindings.is_empty() {
        return false;
    }
    let mut decisions = HashMap::new();
    BindingWalk {
        ir,
        bindings,
        plans: HashSet::new(),
        functions: HashSet::new(),
        decisions: &mut decisions,
        classes: HashSet::new(),
        follow_classes: false,
    }
    .body(root)
}

pub(super) fn class_executes_binding(
    ir: &crate::ir::IrFile,
    class: ClassId,
    bindings: &HashMap<String, Ty>,
) -> bool {
    if bindings.is_empty() {
        return false;
    }
    let mut decisions = HashMap::new();
    BindingWalk {
        ir,
        bindings,
        plans: HashSet::new(),
        functions: HashSet::new(),
        decisions: &mut decisions,
        classes: HashSet::new(),
        follow_classes: true,
    }
    .class(class)
}

struct BindingWalk<'a> {
    ir: &'a crate::ir::IrFile,
    bindings: &'a HashMap<String, Ty>,
    plans: HashSet<u32>,
    functions: HashSet<u32>,
    decisions: &'a mut HashMap<u32, bool>,
    classes: HashSet<ClassId>,
    follow_classes: bool,
}

impl BindingWalk<'_> {
    fn function(&mut self, function: u32) -> bool {
        if let Some(done) = self.decisions.get(&function).copied() {
            return done;
        }
        if !self.functions.insert(function) {
            return false;
        }
        let body = self
            .ir
            .functions
            .get(function as usize)
            .and_then(|function| function.body);
        let used = body.is_some_and(|body| self.body(body));
        self.decisions.insert(function, used);
        used
    }

    fn class(&mut self, class: ClassId) -> bool {
        if !self.classes.insert(class) {
            return false;
        }
        let Some(class_decl) = self.ir.classes.get(class as usize) else {
            return false;
        };
        let mut functions = class_decl.methods.clone();
        functions.extend(
            class_decl
                .properties
                .iter()
                .flat_map(|property| property.getter.into_iter().chain(property.setter)),
        );
        if let Some(extensions) = self.ir.member_ext_props.get(&class_decl.fq_name) {
            functions.extend(
                extensions
                    .iter()
                    .flat_map(|property| std::iter::once(property.getter).chain(property.setter)),
            );
        }
        let mut roots = functions
            .into_iter()
            .filter_map(|function| self.ir.functions.get(function as usize)?.body)
            .collect::<Vec<_>>();
        roots.extend(class_decl.init_body);
        roots.extend(
            self.ir
                .checked_properties
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
        roots.into_iter().any(|root| self.body(root))
    }

    fn body(&mut self, root: ExprId) -> bool {
        let mut pending = vec![root];
        let mut seen = HashSet::new();
        while let Some(expression) = pending.pop() {
            if !seen.insert(expression) {
                continue;
            }
            crate::ir::for_each_child(&self.ir.exprs, expression, &mut |child| pending.push(child));
            if node_uses_binding(self.ir.expr(expression), self.bindings)
                || expression_facts_use_binding(self.ir, expression, self.bindings)
            {
                return true;
            }
            let lambda = match self.ir.expr(expression) {
                IrExpr::Lambda { impl_fn, .. } => Some(*impl_fn),
                _ => None,
            };
            if let Some(implementation) = lambda {
                if self.function(implementation) {
                    return true;
                }
            }
            let nested = if self.follow_classes {
                match self.ir.expr(expression) {
                    IrExpr::New { internal, .. } => self.ir.class_id_by_name(*internal),
                    _ => None,
                }
            } else {
                None
            };
            if let Some(class) = nested {
                if self.class(class) {
                    return true;
                }
            }
            enqueue_local_delegate_plan_bodies(self.ir, expression, &mut pending, &mut self.plans);
        }
        false
    }
}

pub(super) fn enqueue_local_delegate_plan_bodies(
    ir: &crate::ir::IrFile,
    expression: ExprId,
    pending: &mut Vec<ExprId>,
    plans: &mut HashSet<u32>,
) {
    let IrExpr::LocalDelegateAccess(access) = ir.expr(expression) else {
        return;
    };
    if !plans.insert(access.plan) {
        return;
    }
    let Some(plan) = ir.local_delegate_plans.get(access.plan as usize) else {
        return;
    };
    pending.push(plan.getter.body);
    pending.extend(plan.setter.as_ref().map(|setter| setter.body));
}

fn uses_binding(ty: Ty, bindings: &HashMap<String, Ty>) -> bool {
    ty_subst_keep_unbound(ty, bindings) != ty
}

fn substitution_uses_binding(
    substitution: &IrCheckedSubstitution,
    bindings: &HashMap<String, Ty>,
) -> bool {
    substitution.reified
        && (uses_binding(substitution.value, bindings)
            || substitution
                .additional_bounds
                .iter()
                .copied()
                .any(|bound| uses_binding(bound, bindings)))
}

fn substitutions_use_binding(
    substitutions: &[IrCheckedSubstitution],
    bindings: &HashMap<String, Ty>,
) -> bool {
    substitutions
        .iter()
        .any(|substitution| substitution_uses_binding(substitution, bindings))
}

/// Runtime call facts that carry a reified type argument. Static result/signature/storage facts are
/// deliberately excluded: specializing those types may be necessary once a copy exists, but they
/// do not cause a body to execute a reified operation.
fn expression_facts_use_binding(
    ir: &crate::ir::IrFile,
    expression: ExprId,
    bindings: &HashMap<String, Ty>,
) -> bool {
    if ir
        .reified_call_subst
        .get(&expression)
        .is_some_and(|substitutions| {
            substitutions
                .iter()
                .any(|(_, ty)| uses_binding(*ty, bindings))
        })
    {
        return true;
    }
    match ir.expr(expression) {
        IrExpr::Call {
            callee: Callee::External { substitutions, .. },
            ..
        } => substitutions_use_binding(substitutions, bindings),
        IrExpr::Checked(operation) => checked_substitutions(operation)
            .is_some_and(|substitutions| substitutions_use_binding(substitutions, bindings)),
        IrExpr::ReifiedTypeOp { name, .. } | IrExpr::ReifiedClassMarker { name, .. } => {
            bindings.contains_key(name)
        }
        _ => false,
    }
}

fn checked_substitutions(operation: &IrCheckedOperation) -> Option<&[IrCheckedSubstitution]> {
    match operation {
        IrCheckedOperation::Call { substitutions, .. }
        | IrCheckedOperation::ConstructorDelegation { substitutions, .. }
        | IrCheckedOperation::PropertyRead { substitutions, .. }
        | IrCheckedOperation::PropertyWrite { substitutions, .. } => Some(substitutions),
        IrCheckedOperation::PropertyReference { .. }
        | IrCheckedOperation::ExternalPropertyRead { .. }
        | IrCheckedOperation::ExternalPropertyWrite { .. }
        | IrCheckedOperation::LateinitFieldRead { .. }
        | IrCheckedOperation::BackingFieldRead { .. }
        | IrCheckedOperation::BackingFieldWrite { .. }
        | IrCheckedOperation::RangeConstruction { .. }
        | IrCheckedOperation::RangeContains { .. }
        | IrCheckedOperation::IllegalProgressionStep { .. }
        | IrCheckedOperation::RangeLoop { .. } => None,
    }
}

/// Whether this node carries a runtime type operation the reified map would change.
///
/// Static signature slots are not a reason to copy. Specializing the copy applies the same split,
/// so detection does not clone the expression to compare it.
fn node_uses_binding(expression: &IrExpr, bindings: &HashMap<String, Ty>) -> bool {
    match expression {
        IrExpr::TypeOp { type_operand, .. } => uses_binding(*type_operand, bindings),
        IrExpr::KClassLiteral { classifier, .. } => {
            classifier.is_some_and(|ty| uses_binding(ty, bindings))
        }
        IrExpr::Call {
            callee:
                Callee::Intrinsic {
                    operation: IrIntrinsic::TypeOf { ty },
                    ..
                },
            ..
        } => uses_binding(*ty, bindings),
        _ => false,
    }
}
