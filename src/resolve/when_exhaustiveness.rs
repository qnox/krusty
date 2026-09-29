//! `when` exhaustiveness: the branches a `when` without an `else` still misses over a sealed,
//! enum, or `Boolean` subject, and the diagnostic naming them.

use super::scope::FlowExclusion;
use super::{Checker, CheckerScope, ExprLowering};
use crate::ast::{Expr, ExprId, WhenArm};
use crate::types::{Ty, TypeName};

impl Checker<'_> {
    /// Every sealed descendant of `roots`, transitively (the roots themselves included). A sealed class
    /// may nest a further sealed class, so the hierarchy under a sealed subject is a tree, not a list.
    fn sealed_descendants(&self, roots: &[TypeName]) -> std::collections::HashSet<TypeName> {
        let mut seen = std::collections::HashSet::new();
        let mut pending = roots.to_vec();
        while let Some(subclass) = pending.pop() {
            if !seen.insert(subclass) {
                continue;
            }
            if let Some(shape) = self.resolved_type_name(subclass) {
                pending.extend(shape.sealed_subclasses.iter_ids());
            }
        }
        seen
    }

    pub(super) fn when_sealed_missing_branches(
        &self,
        scope: &CheckerScope<'_>,
        subject_expression: Option<ExprId>,
        subject_ty: Option<Ty>,
        arms: &[WhenArm],
    ) -> Option<Vec<String>> {
        let subject = subject_ty?;
        let internal = subject.non_null().obj_internal()?;
        let shape = self.resolved_type_name(internal)?;
        let mut subclasses = shape.sealed_subclasses.iter_ids().collect::<Vec<_>>();
        crate::trace_compiler!(
            "resolve",
            "sealed when subject={subject:?} classifier={internal} subclasses={subclasses:?}",
        );
        if subclasses.is_empty() {
            return None;
        }
        subclasses.sort_by(|left, right| left.path_cmp(*right));
        // Every sealed descendant, not just the direct ones: an `object` arm may name a subclass of a
        // NESTED sealed class (`sealed class Node { sealed class Leaf : Node() … }`), and that arm still
        // covers part of the hierarchy.
        let descendants = self.sealed_descendants(&subclasses);

        let mut covered = std::collections::HashSet::new();
        let mut covers_null = false;
        if let Some(path) = subject_expression.and_then(|subject| self.expr_access_path(subject)) {
            for exclusion in self.lookup_flow_exclusions(scope, &path) {
                match exclusion {
                    FlowExclusion::Classifier(classifier)
                    | FlowExclusion::Singleton(classifier)
                        if descendants.contains(&classifier) =>
                    {
                        covered.insert(classifier);
                    }
                    FlowExclusion::Null => covers_null = true,
                    FlowExclusion::Boolean(_) | FlowExclusion::EnumEntry { .. } => {}
                    FlowExclusion::Classifier(_) | FlowExclusion::Singleton(_) => {}
                }
            }
        }
        for condition in arms
            .iter()
            .filter(|arm| arm.guard.is_none())
            .flat_map(|arm| &arm.conditions)
        {
            let condition = condition.expression();
            match self.file.expr(condition) {
                Expr::Is {
                    ty, negated: false, ..
                } => {
                    let resolved = self
                        .resolved_type_tys
                        .get(&(ty.span.lo, ty.span.hi))
                        .copied()
                        .unwrap_or_else(|| self.type_ref_ty_silent(scope, ty));
                    if let Ty::Obj(internal, _) = resolved {
                        covered.insert(internal);
                    }
                }
                Expr::Name(_) | Expr::Member { .. } => {
                    let object = match self.expr_lowers.get(&condition) {
                        Some(ExprLowering::SingletonValue(singleton)) => Some(singleton.classifier),
                        _ => self.expr_types.get(condition.0 as usize).and_then(|ty| {
                            let internal = ty.non_null().obj_internal()?;
                            self.resolved_type_name(internal)
                                .is_some_and(|shape| shape.is_object())
                                .then_some(internal)
                        }),
                    };
                    if let Some(object) = object.filter(|internal| descendants.contains(internal)) {
                        covered.insert(object);
                    }
                }
                Expr::NullLit => covers_null = true,
                _ => {}
            }
        }

        // A sealed subclass that is ITSELF sealed is covered when all of ITS subclasses are: the
        // hierarchy is a tree, and only its LEAVES can be instantiated. `sealed class Node { sealed
        // class Leaf : Node(); … }` covered by `IntLeaf`/`StrLeaf`/`Branch` is exhaustive, and
        // demanding `is Leaf` asked for a branch kotlinc rejects as redundant. The depth cap only
        // guards against a malformed hierarchy looping.
        fn uncovered_leaves(
            checker: &Checker<'_>,
            subclass: TypeName,
            covered: &std::collections::HashSet<TypeName>,
            depth: u32,
            out: &mut Vec<TypeName>,
        ) {
            if covered.contains(&subclass) {
                return;
            }
            let nested: Vec<TypeName> = (depth < 16)
                .then(|| checker.resolved_type_name(subclass))
                .flatten()
                .map(|shape| shape.sealed_subclasses.iter_ids().collect())
                .unwrap_or_default();
            if nested.is_empty() {
                out.push(subclass);
                return;
            }
            for child in nested {
                uncovered_leaves(checker, child, covered, depth + 1, out);
            }
        }
        let mut uncovered = Vec::new();
        for subclass in subclasses {
            uncovered_leaves(self, subclass, &covered, 0, &mut uncovered);
        }
        uncovered.sort_by(|left, right| left.path_cmp(*right));
        uncovered.dedup();
        let mut missing = uncovered
            .into_iter()
            .map(|subclass| {
                let name = subclass.nested_segment_ref();
                if self
                    .resolved_type_name(subclass)
                    .is_some_and(|shape| shape.is_object())
                {
                    name.to_string()
                } else {
                    format!("is {name}")
                }
            })
            .collect::<Vec<_>>();
        if subject.is_nullable() && !covers_null {
            missing.push("null".to_string());
        }
        Some(missing)
    }

    pub(super) fn when_enum_missing_branches(
        &self,
        scope: &CheckerScope<'_>,
        subject_expression: Option<ExprId>,
        subject_ty: Option<Ty>,
        arms: &[WhenArm],
    ) -> Option<Vec<String>> {
        let subject = subject_ty?;
        let internal = subject.non_null().obj_internal()?;
        let entries = self.resolved_type_name(internal)?.enum_entries.clone();
        if entries.is_empty() {
            return None;
        }

        let mut covered = std::collections::HashSet::new();
        let mut covers_null = false;
        if let Some(path) = subject_expression.and_then(|subject| self.expr_access_path(subject)) {
            for exclusion in self.lookup_flow_exclusions(scope, &path) {
                match exclusion {
                    FlowExclusion::EnumEntry { classifier, name } if classifier == internal => {
                        covered.insert(name);
                    }
                    FlowExclusion::Null => covers_null = true,
                    FlowExclusion::Boolean(_)
                    | FlowExclusion::EnumEntry { .. }
                    | FlowExclusion::Singleton(_)
                    | FlowExclusion::Classifier(_) => {}
                }
            }
        }
        for condition in arms
            .iter()
            .filter(|arm| arm.guard.is_none())
            .flat_map(|arm| &arm.conditions)
        {
            let condition = condition.expression();
            if let Some(entry) = self
                .resolved_enum_entries
                .get(&condition)
                .filter(|entry| entry.classifier == internal)
            {
                covered.insert(entry.name.clone());
            } else if matches!(self.file.expr(condition), Expr::NullLit) {
                covers_null = true;
            }
        }

        let mut missing = entries
            .into_iter()
            .filter(|entry| !covered.contains(entry))
            .collect::<Vec<_>>();
        if subject.is_nullable() && !covers_null {
            missing.push("null".to_string());
        }
        Some(missing)
    }

    pub(super) fn when_boolean_missing_branches(
        &self,
        scope: &CheckerScope<'_>,
        subject_expression: Option<ExprId>,
        subject_ty: Option<Ty>,
        arms: &[WhenArm],
    ) -> Option<Vec<String>> {
        let subject = subject_ty?;
        if subject.non_null() != Ty::Boolean {
            return None;
        }
        let mut covered = std::collections::HashSet::new();
        let mut covers_null = false;
        if let Some(path) = subject_expression.and_then(|subject| self.expr_access_path(subject)) {
            for exclusion in self.lookup_flow_exclusions(scope, &path) {
                match exclusion {
                    FlowExclusion::Boolean(value) => {
                        covered.insert(value);
                    }
                    FlowExclusion::Null => covers_null = true,
                    FlowExclusion::EnumEntry { .. }
                    | FlowExclusion::Singleton(_)
                    | FlowExclusion::Classifier(_) => {}
                }
            }
        }
        for condition in arms
            .iter()
            .filter(|arm| arm.guard.is_none())
            .flat_map(|arm| &arm.conditions)
        {
            match self.file.expr(condition.expression()) {
                Expr::BoolLit(value) => {
                    covered.insert(*value);
                }
                Expr::NullLit => covers_null = true,
                _ => {}
            }
        }
        let mut missing = [false, true]
            .into_iter()
            .filter(|value| !covered.contains(value))
            .map(|value| value.to_string())
            .collect::<Vec<_>>();
        if subject.is_nullable() && !covers_null {
            missing.push("null".to_string());
        }
        Some(missing)
    }

    pub(super) fn non_exhaustive_when_message(missing: Option<Vec<String>>) -> String {
        let Some(missing) = missing.filter(|branches| !branches.is_empty()) else {
            return "'when' expression must be exhaustive. Add an 'else' branch.".to_string();
        };
        let branches = missing
            .iter()
            .map(|branch| format!("'{branch}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let noun = if missing.len() == 1 {
            "branch"
        } else {
            "branches"
        };
        format!(
            "'when' expression must be exhaustive. Add the {branches} {noun} or an 'else' branch."
        )
    }
}

impl super::TypeInfo {
    /// Whether the checker proved the `when` at `expression` exhaustive, by an `else` branch or by
    /// covering every value of its subject.
    pub fn when_is_exhaustive(&self, expression: ExprId) -> bool {
        self.exhaustive_whens.contains(&expression)
    }
}
