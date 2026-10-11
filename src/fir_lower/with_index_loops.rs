//! kotlinc's `WithIndexLoopHeader`: a loop over `x.withIndex()` that destructures its
//! `IndexedValue` in the loop header is the loop over `x`, with an index beside it.
//!
//! The loop over `x` is the one checked FIR built for the receiver. Each iteration binds the
//! destructuring's index entries first, then steps an index the loop counts on its own, then binds
//! the element entries: the first reads the element and the others copy it. An array or a
//! `String` counts from 0 by 1, so its counter is the index; an iterator counts its own index; a
//! counted loop's backend decides which of the two its counter is.

use crate::fir::{
    ControlTargetId, FirDestructureEntry, FirExprId, FirIndexedValueComponent, FirLoopHeader,
    FirStatementKind, FirWithIndexLoop, LocalValueId, ResolvedTy,
};
use crate::ir::{ExprId, IrBinOp, IrConst, IrExpr, IrLoopIndex};
use crate::types::Ty;

use super::loops::{IterableLoopContract, IteratorLoopContract, LoopBinding, RangeLoopContract};
use super::{BodyLowering, FirLoweringFailure};

/// One destructuring entry bound by a `withIndex()` loop.
#[derive(Clone, Copy)]
struct ComponentBinding {
    component: FirIndexedValueComponent,
    target: LocalValueId,
    ty: ResolvedTy,
    /// The type of the component the entry reads, before its conversion.
    source_ty: Ty,
    conversion: Option<crate::fir::FirConversion>,
}

impl BodyLowering<'_> {
    pub(super) fn with_index_loop(
        &mut self,
        target: ControlTargetId,
        with_index: &FirWithIndexLoop,
        body: FirExprId,
    ) -> Result<ExprId, FirLoweringFailure> {
        let binding = LoopBinding::WithIndex(with_index);
        match &with_index.nested {
            FirLoopHeader::Iterable {
                variable_ty,
                kind,
                iterable,
                ..
            } => self.iterable_loop(IterableLoopContract {
                target,
                binding,
                variable_ty: *variable_ty,
                kind,
                iterable: *iterable,
                body,
            }),
            FirLoopHeader::Iterator {
                variable_ty,
                iterable,
                iterator_ty,
                iterator,
                has_next,
                next,
                ..
            } => self.iterator_loop(IteratorLoopContract {
                target,
                binding,
                variable_ty: *variable_ty,
                iterable: *iterable,
                iterator_ty: *iterator_ty,
                iterator,
                has_next,
                next,
                body,
            }),
            FirLoopHeader::Progression {
                counter,
                source,
                unsigned_compare,
                ..
            } => self.range_loop(RangeLoopContract {
                target,
                binding,
                counter: *counter,
                source: std::borrow::Cow::Borrowed(source),
                unsigned_compare: unsigned_compare.as_ref(),
                body,
            }),
            FirLoopHeader::While { .. }
            | FirLoopHeader::DoWhile { .. }
            | FirLoopHeader::Range { .. }
            | FirLoopHeader::WithIndex(_) => Err(FirLoweringFailure::MalformedWithIndexLoop(
                with_index.destructure,
            )),
        }
    }

    /// The receiver of `withIndex()` as the `Iterable` or `Sequence` whose `iterator()` the loop
    /// calls (`DefaultIterableHandler`, `DefaultSequenceHandler`): an implicit cast when it is
    /// typed as anything else.
    pub(super) fn with_index_receiver(
        &mut self,
        value: ExprId,
        value_ty: Ty,
        iterator: &crate::fir::FirIteratorCall,
    ) -> (ExprId, Ty) {
        let crate::fir::FirCallTarget::External {
            receiver: Some(receiver),
            ..
        } = &iterator.target
        else {
            return (value, value_ty);
        };
        let receiver = receiver.get();
        if receiver == value_ty {
            return (value, value_ty);
        }
        let cast = self.ir.add_expr(IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::Cast,
            arg: value,
            type_operand: receiver,
        });
        (cast, receiver)
    }

    /// `next()` read for its effect only: no element is declared, so the call keeps its declared
    /// result instead of the element type a declaration would cast it to (`IterableLoopHeader`).
    pub(super) fn undeclared_next(
        next: &crate::fir::FirIteratorCall,
    ) -> crate::fir::FirIteratorCall {
        let mut next = next.clone();
        next.result_check = None;
        if let crate::fir::FirCallTarget::External {
            result,
            declared_result: Some(declared),
            ..
        } = &mut next.target
        {
            *result = *declared;
        }
        next
    }

    /// `var index = 0`, the index a loop counts on its own.
    pub(super) fn index_declaration(&mut self, with_index: &FirWithIndexLoop) -> ExprId {
        let zero = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        self.ir.add_expr(IrExpr::Variable {
            index: self.value_slot(with_index.index),
            ty: Ty::Int,
            init: Some(zero),
            named: false,
        })
    }

    /// `initializeIteration` over a loop that reads `element` itself: the index entries, the
    /// index's own step when `counts_index`, then the element entries, or the bare element read
    /// when no entry binds it.
    pub(super) fn with_index_iteration(
        &mut self,
        with_index: &FirWithIndexLoop,
        element: ExprId,
        counts_index: bool,
    ) -> Result<Vec<ExprId>, FirLoweringFailure> {
        let components = self.component_bindings(with_index)?;
        let mut statements = self.index_bindings(with_index, &components)?;
        if counts_index {
            statements.push(self.index_step(with_index));
        }
        let mut values = components
            .iter()
            .filter(|binding| binding.component == FirIndexedValueComponent::Value);
        match values.next() {
            Some(first) => {
                statements.push(self.component_declaration(*first, element)?);
                for copy in values {
                    let read = self
                        .ir
                        .add_expr(IrExpr::GetValue(self.value_slot(first.target)));
                    statements.push(self.component_declaration(*copy, read)?);
                }
            }
            None => statements.push(element),
        }
        Ok(statements)
    }

    /// The index and element of a counted loop: its backend declares the element as the loop
    /// variable, the first element entry when there is one, and decides whether its counter is
    /// the index.
    pub(super) fn counted_with_index(
        &mut self,
        with_index: &FirWithIndexLoop,
    ) -> Result<(LocalValueId, IrLoopIndex), FirLoweringFailure> {
        let components = self.component_bindings(with_index)?;
        let declaration = self.index_declaration(with_index);
        let bindings = self.index_bindings(with_index, &components)?;
        let bindings = self.ir.add_expr(IrExpr::Block {
            stmts: bindings,
            value: None,
        });
        let mut values = components
            .iter()
            .filter(|binding| binding.component == FirIndexedValueComponent::Value);
        let first = values.next().copied();
        let mut copies = Vec::new();
        if let Some(first) = first {
            for copy in values {
                let read = self
                    .ir
                    .add_expr(IrExpr::GetValue(self.value_slot(first.target)));
                copies.push(self.component_declaration(*copy, read)?);
            }
        }
        let element_copies = self.ir.add_expr(IrExpr::Block {
            stmts: copies,
            value: None,
        });
        let element = match (&first, &with_index.nested) {
            (Some(first), _) => first.target,
            (None, FirLoopHeader::Progression { variable, .. }) => *variable,
            (None, _) => {
                return Err(FirLoweringFailure::MalformedWithIndexLoop(
                    with_index.destructure,
                ))
            }
        };
        Ok((
            element,
            IrLoopIndex {
                declaration,
                bindings,
                element_bound: first.is_some(),
                element_copies,
            },
        ))
    }

    /// The declarations of the index entries, each reading the index.
    fn index_bindings(
        &mut self,
        with_index: &FirWithIndexLoop,
        components: &[ComponentBinding],
    ) -> Result<Vec<ExprId>, FirLoweringFailure> {
        components
            .iter()
            .filter(|binding| binding.component == FirIndexedValueComponent::Index)
            .map(|binding| {
                let read = self
                    .ir
                    .add_expr(IrExpr::GetValue(self.value_slot(with_index.index)));
                self.component_declaration(*binding, read)
            })
            .collect()
    }

    /// `index = index + 1`, which kotlinc builds without an increment's origin.
    fn index_step(&mut self, with_index: &FirWithIndexLoop) -> ExprId {
        let slot = self.value_slot(with_index.index);
        let current = self.ir.add_expr(IrExpr::GetValue(slot));
        let one = self.ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let next = self.ir.add_arithmetic(IrBinOp::Add, current, one, Ty::Int);
        let step = self.ir.add_expr(IrExpr::SetValue {
            var: slot,
            value: next,
        });
        self.ir.plain_updates.insert(step);
        step
    }

    /// `val name = value` for one destructuring entry, converted as the entry declares.
    fn component_declaration(
        &mut self,
        binding: ComponentBinding,
        value: ExprId,
    ) -> Result<ExprId, FirLoweringFailure> {
        let value = self.lowered_with_conversion(value, binding.source_ty, binding.conversion)?;
        let initializer = if self.shared_local_type(binding.target).is_some() {
            self.shared_cell_new(binding.ty, Some(value))
        } else {
            value
        };
        let declaration = self.ir.add_expr(IrExpr::Variable {
            index: self.value_slot(binding.target),
            ty: binding.ty.get(),
            init: Some(initializer),
            named: true,
        });
        if let Some(name) = self.body.debug_value_name(binding.target) {
            self.ir.value_names.insert(declaration, name.to_owned());
        }
        Ok(declaration)
    }

    /// The bound entries of the consumed destructuring, in source order, with the component each
    /// reads.
    fn component_bindings(
        &self,
        with_index: &FirWithIndexLoop,
    ) -> Result<Vec<ComponentBinding>, FirLoweringFailure> {
        let malformed = || FirLoweringFailure::MalformedWithIndexLoop(with_index.destructure);
        let Some(FirStatementKind::Destructure { entries, .. }) = self
            .body
            .statement(with_index.destructure)
            .map(|statement| &statement.kind)
        else {
            return Err(malformed());
        };
        if entries.len() != with_index.components.len() {
            return Err(malformed());
        }
        let mut bindings = Vec::new();
        for (entry, component) in entries.iter().zip(&with_index.components) {
            if let FirDestructureEntry::Binding {
                target,
                ty,
                component: read,
                conversion,
                ..
            } = *entry
            {
                let source_ty = self
                    .body
                    .expr(read)
                    .ok_or(FirLoweringFailure::MissingExpression(read))?
                    .ty
                    .get();
                bindings.push(ComponentBinding {
                    component: component.ok_or_else(malformed)?,
                    target,
                    ty,
                    source_ty,
                    conversion,
                });
            }
        }
        Ok(bindings)
    }
}
