//! Semantic non-null facts needed before bytecode constants are interned.
//!
//! The finished-method CFG pass removes guards made redundant only by emitted control flow. This
//! earlier boundary answers a different question: whether checked IR already guarantees that the
//! cast operand cannot be null. Avoiding the guard here also avoids dead diagnostic strings and
//! method references in the class pool, which is part of kotlinc byte equality.

use std::collections::{HashMap, HashSet};

use crate::ir::{IrExpr, IrFile, IrTypeOp};

use super::Emitter;

/// Every value stored into each semantic local of one emitted body.
#[derive(Default)]
pub(super) struct ValueStores {
    stores: HashMap<u32, Vec<Store>>,
}

/// One value stored into a semantic local.
#[derive(Clone, Copy)]
struct Store {
    value: u32,
    /// A declaration's initializer is adapted to the local's slot type before the store; an
    /// assignment stores the value as emitted.
    adapted: bool,
}

impl ValueStores {
    /// Collect stores reachable inside one body. A lambda owns its own value-index domain; only its
    /// capture expressions belong to the enclosing body's proof.
    pub(super) fn collect(ir: &IrFile, roots: &[u32]) -> Self {
        let mut stores = Self::default();
        let mut seen = HashSet::new();
        let mut pending = roots.to_vec();
        while let Some(expression) = pending.pop() {
            if !seen.insert(expression) {
                continue;
            }
            match ir.expr(expression) {
                IrExpr::Variable { index, init, .. } => {
                    stores
                        .stores
                        .entry(*index)
                        .or_default()
                        .extend(init.iter().map(|&value| Store {
                            value,
                            adapted: true,
                        }));
                }
                IrExpr::SetValue { var, value } => {
                    stores.stores.entry(*var).or_default().push(Store {
                        value: *value,
                        adapted: false,
                    });
                }
                _ => {}
            }
            match ir.expr(expression) {
                IrExpr::Lambda { captures, .. } => pending.extend(captures.iter().copied()),
                _ => crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
                    pending.push(child)
                }),
            }
        }
        stores
    }

    fn values(&self, value: u32) -> Option<&[Store]> {
        self.stores.get(&value).map(Vec::as_slice)
    }
}

impl Emitter<'_> {
    /// Whether checked IR itself proves this operand non-null.
    pub(super) fn semantic_non_null(&self, expression: u32) -> bool {
        self.semantic_non_null_within(expression, &mut HashSet::new(), &mut HashSet::new())
    }

    fn semantic_non_null_within(
        &self,
        expression: u32,
        visiting_expressions: &mut HashSet<u32>,
        visiting_values: &mut HashSet<u32>,
    ) -> bool {
        if !visiting_expressions.insert(expression) {
            return false;
        }
        let result = match self.ir.expr(expression) {
            IrExpr::Const(constant) => !matches!(constant, crate::ir::IrConst::Null),
            IrExpr::New { .. }
            | IrExpr::ClassConst { .. }
            | IrExpr::KClassLiteral { .. }
            | IrExpr::NotNullAssert { .. }
            | IrExpr::Throw { .. } => true,
            IrExpr::TypeOp { op, arg, .. } => match op {
                IrTypeOp::CastNonNull => true,
                IrTypeOp::Cast | IrTypeOp::ImplicitCoercion => {
                    self.semantic_non_null_within(*arg, visiting_expressions, visiting_values)
                }
                _ => false,
            },
            IrExpr::Block {
                value: Some(value), ..
            } => self.semantic_non_null_within(*value, visiting_expressions, visiting_values),
            IrExpr::When { branches } => {
                branches.iter().any(|(condition, _)| condition.is_none())
                    && branches.iter().all(|&(_, value)| {
                        self.semantic_non_null_within(value, visiting_expressions, visiting_values)
                    })
            }
            IrExpr::GetValue(value) => {
                self.semantic_value_non_null(*value, visiting_expressions, visiting_values)
            }
            _ => false,
        };
        visiting_expressions.remove(&expression);
        result
    }

    fn semantic_value_non_null(
        &self,
        value: u32,
        visiting_expressions: &mut HashSet<u32>,
        visiting_values: &mut HashSet<u32>,
    ) -> bool {
        if self.checked_parameters.contains(&value) {
            return true;
        }
        if !visiting_values.insert(value) {
            return false;
        }
        let result = self.value_stores.stores.get(&value).is_some_and(|stores| {
            !stores.is_empty()
                && stores.iter().all(|stored| {
                    self.semantic_non_null_within(
                        stored.value,
                        visiting_expressions,
                        visiting_values,
                    )
                })
        });
        visiting_values.remove(&value);
        result
    }

    /// Narrow one semantic local's StackMapTable type when every value ever stored into it has the
    /// same exact JVM reference identity. The LVT keeps the source-declared type; verifier frames,
    /// like kotlinc's, carry the more precise fact established on every incoming edge.
    pub(super) fn semantic_local_frame_ty(
        &self,
        value: u32,
        declared: crate::types::Ty,
    ) -> crate::types::Ty {
        if !declared.is_reference() {
            return declared;
        }
        self.common_stored_reference_ty(value, declared, &mut HashSet::new())
            .unwrap_or(declared)
    }

    fn common_stored_reference_ty(
        &self,
        value: u32,
        declared: crate::types::Ty,
        visiting_values: &mut HashSet<u32>,
    ) -> Option<crate::types::Ty> {
        if !visiting_values.insert(value) {
            return None;
        }
        let Some(stores) = self.value_stores.values(value) else {
            visiting_values.remove(&value);
            return None;
        };
        let mut common = None;
        let mut saw_null = false;
        for &stored in stores {
            let Some(ty) = self.stored_reference_ty(stored, declared, visiting_values) else {
                visiting_values.remove(&value);
                return None;
            };
            if ty == crate::types::Ty::Null {
                saw_null = true;
                continue;
            }
            if !ty.is_reference() {
                visiting_values.remove(&value);
                return None;
            }
            if common.is_some_and(|current| {
                crate::jvm::names::type_descriptor(current)
                    != crate::jvm::names::type_descriptor(ty)
            }) {
                visiting_values.remove(&value);
                return None;
            }
            common = Some(ty);
        }
        visiting_values.remove(&value);
        common.or_else(|| saw_null.then_some(crate::types::Ty::Null))
    }

    /// The verifier type one store leaves in its local. An adapted initializer goes through the
    /// same reference coercion the emitter applies: a `checkcast` leaves the slot's own type
    /// (`val g: Greeter = Ann()` stores a `Greeter`, as in kotlinc), and a carrier adaptation is not
    /// modelled, so it keeps the declared type.
    fn stored_reference_ty(
        &self,
        stored: Store,
        declared: crate::types::Ty,
        visiting_values: &mut HashSet<u32>,
    ) -> Option<crate::types::Ty> {
        let ty = self.expression_reference_ty(stored.value, visiting_values)?;
        if !stored.adapted || ty == crate::types::Ty::Null {
            return Some(ty);
        }
        match self.reference_coercion(self.value_ty(stored.value), declared) {
            super::operand_representation::ReferenceCoercion::Unchanged => Some(ty),
            super::operand_representation::ReferenceCoercion::Cast(_) => Some(declared),
            super::operand_representation::ReferenceCoercion::Carried => None,
        }
    }

    fn expression_reference_ty(
        &self,
        expression: u32,
        visiting_values: &mut HashSet<u32>,
    ) -> Option<crate::types::Ty> {
        match self.ir.expr(expression) {
            IrExpr::GetValue(value) => {
                let &(_, declared) = self.slots.get(value)?;
                self.common_stored_reference_ty(*value, declared, visiting_values)
            }
            IrExpr::Block {
                value: Some(value), ..
            } => self.expression_reference_ty(*value, visiting_values),
            IrExpr::When { branches } => {
                let mut common = None;
                let mut saw_null = false;
                for &(_, value) in branches {
                    let ty = self.expression_reference_ty(value, visiting_values)?;
                    if ty == crate::types::Ty::Null {
                        saw_null = true;
                        continue;
                    }
                    if common.is_some_and(|current| {
                        crate::jvm::names::type_descriptor(current)
                            != crate::jvm::names::type_descriptor(ty)
                    }) {
                        return None;
                    }
                    common = Some(ty);
                }
                common.or_else(|| saw_null.then_some(crate::types::Ty::Null))
            }
            _ => {
                let ty = self.value_ty(expression);
                (ty == crate::types::Ty::Null || ty.is_reference()).then_some(ty)
            }
        }
    }
}
