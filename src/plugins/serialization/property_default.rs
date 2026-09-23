//! A serializable property's checked default, re-framed for the method that writes the object.
//!
//! kotlinc's `write$Self` compares a defaulted element with its initializer, evaluated again at the
//! write: `shouldEncodeElementDefault(desc, i) || self.tags != listOf("a")`. The initializer was
//! checked in the constructor's frame, where an earlier property is a constructor PARAMETER; in the
//! writing method it is a property of the object being written, which is where kotlinc reads it
//! from. This module owns that one re-framing so the write path never guesses at a parameter's slot.

use crate::ir::{ExprId, IrExpr, IrFile};
use crate::types::TypeName;

/// How the writing method reaches what the constructor saw as parameters and locals.
pub(super) struct WriteFrame<'a> {
    /// The first value index the writing method has not claimed; the initializer's own locals are
    /// moved there.
    pub(super) first_free_local: u32,
    /// Read constructor property `index` off the object being written.
    pub(super) read_property: &'a mut dyn FnMut(&mut IrFile, usize) -> Option<ExprId>,
    /// The line of the property whose default this is. The initializer's own code is attributed to
    /// it, as it is where the constructor applies the same default.
    pub(super) property_line: Option<u32>,
    /// The line a re-read property is attributed to: it is the writing method's read, not source
    /// the initializer contains.
    pub(super) read_line: Option<u32>,
}

/// A private copy of constructor property `index`'s checked default, evaluated in `frame`.
///
/// `None` when the property declares no retained default, or when the initializer refers to a
/// value the writing method cannot reach (the constructor receiver itself).
pub(super) fn default_in_write_frame(
    ir: &mut IrFile,
    owner: TypeName,
    index: usize,
    frame: WriteFrame<'_>,
) -> Option<ExprId> {
    let defaults = ir.class_ctor_defaults_name(owner)?;
    // The constructor frame: slot 0 is the receiver and parameter `k` is slot `k + 1`, the numbering
    // the deserialization constructor relies on for the same expressions.
    let parameter_count = u32::try_from(defaults.len()).ok()?;
    let initializer = defaults.get(index).copied().flatten()?;
    let (copy, _) = crate::ir::clone_expression_dag(ir, initializer);

    let mut reads = Vec::new();
    let mut locals = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut pending = vec![copy];
    while let Some(expression) = pending.pop() {
        if !visited.insert(expression) {
            continue;
        }
        match &ir.exprs[expression as usize] {
            IrExpr::GetValue(0) => return None,
            IrExpr::GetValue(slot) if *slot <= parameter_count => {
                reads.push((expression, *slot - 1));
            }
            IrExpr::GetValue(_)
            | IrExpr::SetValue { .. }
            | IrExpr::Variable { .. }
            | IrExpr::Try { .. } => locals.push(expression),
            _ => {}
        }
        // A lambda's inline body is numbered in the lambda's own frame; only its captures belong
        // to the enclosing one.
        if let IrExpr::Lambda { captures, .. } = &ir.exprs[expression as usize] {
            pending.extend(captures.iter().copied());
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }

    let first_constructor_local = parameter_count + 1;
    let relocate = |slot: &mut u32| {
        if *slot >= first_constructor_local {
            *slot = *slot - first_constructor_local + frame.first_free_local;
        }
    };
    for expression in locals {
        match &mut ir.exprs[expression as usize] {
            IrExpr::GetValue(slot)
            | IrExpr::SetValue { var: slot, .. }
            | IrExpr::Variable { index: slot, .. } => relocate(slot),
            IrExpr::Try { catches, .. } => catches.iter_mut().for_each(|c| relocate(&mut c.var)),
            _ => {}
        }
    }
    if let Some(line) = frame.property_line {
        for &expression in &visited {
            ir.expr_source_lines.entry(expression).or_insert(line);
        }
    }
    for (expression, parameter) in reads {
        let read = (frame.read_property)(ir, parameter as usize)?;
        // The copied node keeps its identity (its parents point at it); it becomes the read.
        ir.exprs[expression as usize] = ir.exprs[read as usize].clone();
        match frame.read_line {
            Some(line) => ir.expr_source_lines.insert(expression, line),
            None => ir.expr_source_lines.remove(&expression),
        };
    }
    Some(copy)
}
