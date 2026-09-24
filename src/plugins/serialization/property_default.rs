//! A serializable property's checked default, re-framed for a generated method that applies or
//! compares it.
//!
//! A property's default is its constructor parameter's default or, for a property declared in the
//! class body, its initializer. Either one makes the element optional, and kotlinc evaluates it again
//! where it is needed:
//! - the deserialization constructor applies it when the element was absent;
//! - `write$Self` compares the value with it, as in
//!   `shouldEncodeElementDefault(desc, i) || self.tags != listOf("a")`.
//!
//! Both kinds of default were checked in the primary constructor's frame. There the receiver is
//! value 0, constructor parameter `k` is value `k + 1`, and an earlier property is a constructor
//! PARAMETER. In the generated method that property is a property of the object being built or
//! written, which is where kotlinc reads it from. This module owns that re-framing, so neither
//! method guesses at a parameter's slot.

use crate::ir::{ClassId, ExprId, IrExpr, IrFile, IrLocalPropertyLayout};

/// The checked default of property `field` of `class_id`: its constructor parameter's default, or
/// the initializer of a property declared in the class body. `None` when it declares neither.
pub(super) fn checked_default(ir: &IrFile, class_id: ClassId, field: usize) -> Option<ExprId> {
    checked_default_in_scope(ir, class_id, field).map(|(default, _)| default)
}

/// [`checked_default`] with the number of constructor parameters in its scope. A parameter's
/// default sees only the parameters before it, and its own locals are numbered right after them,
/// so a value past that count is one of its locals even where a later parameter has the same
/// number. A body property's initializer sees every parameter.
fn checked_default_in_scope(ir: &IrFile, class_id: ClassId, field: usize) -> Option<(ExprId, u32)> {
    let class = &ir.classes[class_id as usize];
    let field_index = u32::try_from(field).ok()?;
    if let Some(parameter) = class
        .ctor_args
        .iter()
        .position(|argument| argument.is_field && argument.field_index == Some(field_index))
    {
        if !class.ctor_args[parameter].has_default {
            return None;
        }
        let default = ir
            .class_ctor_defaults_name(class.fq_name_id())
            .and_then(|defaults| defaults.get(parameter).copied().flatten())
            .expect("a constructor property declaring a default retains its lowered expression");
        return Some((default, u32::try_from(parameter).ok()?));
    }
    // A body property's initializer is composed into the primary constructor, in its frame. A class
    // without one runs it in a secondary constructor's frame instead, which no generated member here
    // shares.
    if !class.has_primary_ctor {
        return None;
    }
    let parameters = u32::try_from(class.ctor_args.len()).ok()?;
    // A constructor property's field was answered above, so the property whose backing field this
    // is was declared in the body.
    let initializer = ir.checked_properties.iter().find_map(|(id, property)| {
        match ir.local_property_layouts.get(id) {
            Some(IrLocalPropertyLayout::Member {
                class,
                backing_field: Some(backing),
                ..
            }) if *class == class_id && *backing == field_index => property.initializer,
            _ => None,
        }
    })?;
    Some((initializer, parameters))
}

/// How the method a default is copied into reaches what the constructor saw.
pub(super) struct DefaultFrame<'a> {
    /// The first value index the method has not claimed; the initializer's own locals are moved
    /// there.
    pub(super) first_free_local: u32,
    /// Read property (by FIELD index) of the object the method builds or writes.
    pub(super) read_field: &'a mut dyn FnMut(&mut IrFile, usize) -> Option<ExprId>,
    /// The value holding that object, when the method is a member of its class and may reach its
    /// private state. `None` leaves a default that uses the receiver underivable there.
    pub(super) receiver: Option<u32>,
    /// The line of the property whose default this is. The initializer's own code is attributed to
    /// it, as it is where the constructor applies the same default.
    pub(super) property_line: Option<u32>,
    /// The line a re-read property is attributed to: it is the method's own read, not source the
    /// initializer contains. `None` leaves it unattributed.
    pub(super) read_line: Option<u32>,
}

/// A private copy of property `field`'s checked default, evaluated in `frame`.
///
/// `None` when the property declares no default, or when the default refers to a value the method
/// cannot reach.
pub(super) fn default_in_frame(
    ir: &mut IrFile,
    class_id: ClassId,
    field: usize,
    frame: DefaultFrame<'_>,
) -> Option<ExprId> {
    let (initializer, parameter_count) = checked_default_in_scope(ir, class_id, field)?;
    // Constructor parameter `k` is value `k + 1`; each one a default can read is a property.
    let parameter_fields = ir.classes[class_id as usize]
        .ctor_args
        .iter()
        .take(parameter_count as usize)
        .map(|argument| argument.is_field.then_some(argument.field_index).flatten())
        .collect::<Vec<_>>();
    let (copy, _) = crate::ir::clone_expression_dag(ir, initializer);

    let mut reads = Vec::new();
    let mut receivers = Vec::new();
    let mut locals = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut pending = vec![copy];
    while let Some(expression) = pending.pop() {
        if !visited.insert(expression) {
            continue;
        }
        match &ir.exprs[expression as usize] {
            IrExpr::GetValue(0) => receivers.push(expression),
            IrExpr::GetValue(slot) if *slot <= parameter_count => {
                let field = parameter_fields[*slot as usize - 1]?;
                reads.push((expression, field));
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
    if !receivers.is_empty() {
        let receiver = frame.receiver?;
        for expression in receivers {
            ir.exprs[expression as usize] = IrExpr::GetValue(receiver);
        }
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
    for (expression, field) in reads {
        let read = (frame.read_field)(ir, field as usize)?;
        // The copied node keeps its identity (its parents point at it); it becomes the read.
        ir.exprs[expression as usize] = ir.exprs[read as usize].clone();
        match frame.read_line {
            Some(line) => ir.expr_source_lines.insert(expression, line),
            None => ir.expr_source_lines.remove(&expression),
        };
    }
    Some(copy)
}
