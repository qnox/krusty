//! kotlinc's `FlattenStringConcatenationLowering` for a `toString()` operand: a concatenation
//! appends the receiver of a `toString()` call (`Any.toString` or any override of it, never a
//! `super` call) itself, since appending a value already converts it to its string.
//!
//! On the JVM, kotlinc runs this after `JvmInlineClassLowering`, so a value-class `toString()`
//! has already become a static `toString-impl` call and keeps its call. This pass runs after the
//! value-class pass for the same reason; a static call has no dispatch receiver and is left alone.

use std::collections::HashMap;

use crate::fir::CallableId;
use crate::ir::{Callee, ExprId, FunId, IrExpr, IrFile};
use crate::types::SemanticCallRole;

pub(crate) fn flatten_to_string_operands(ir: &mut IrFile) {
    let roles = CallRoles::new(ir);
    let mut rewrites = Vec::new();
    for (index, expression) in ir.exprs.iter().enumerate() {
        let IrExpr::StringConcat(parts) = expression else {
            continue;
        };
        if !parts
            .iter()
            .any(|&part| roles.to_string_receiver(ir, part).is_some())
        {
            continue;
        }
        let mut pending = parts.iter().rev().copied().collect::<Vec<_>>();
        let mut flattened = Vec::with_capacity(pending.len());
        while let Some(part) = pending.pop() {
            match roles.to_string_receiver(ir, part) {
                Some(receiver) => match ir.expr(receiver) {
                    // A concatenation receiver contributes its own parts, as kotlinc collects them.
                    IrExpr::StringConcat(inner) => pending.extend(inner.iter().rev().copied()),
                    _ => pending.push(receiver),
                },
                None => flattened.push(part),
            }
        }
        rewrites.push((index, flattened));
    }
    for (index, parts) in rewrites {
        ir.exprs[index] = IrExpr::StringConcat(parts);
    }
}

/// The language role each call plays, from the facts the frontend published: a dependency call
/// keeps its own, and a current-module function inherits one through the declaration it overrides.
struct CallRoles {
    callables: HashMap<FunId, CallableId>,
}

impl CallRoles {
    fn new(ir: &IrFile) -> Self {
        Self {
            callables: ir
                .checked_callable_functions
                .iter()
                .map(|(&callable, &function)| (function, callable))
                .collect(),
        }
    }

    /// The receiver of a virtual, argument-free call that plays the `Any.toString` role.
    fn to_string_receiver(&self, ir: &IrFile, expression: ExprId) -> Option<ExprId> {
        let (receiver, callable) = match ir.expr(expression) {
            IrExpr::Call {
                callee: Callee::Virtual { module_target, .. },
                dispatch_receiver: Some(receiver),
                args,
            } if args.is_empty() => (*receiver, *module_target),
            IrExpr::MethodCall {
                class,
                index,
                receiver,
                args,
            } if args.is_empty() => {
                let function = ir.classes[*class as usize].methods[*index as usize];
                (*receiver, self.callables.get(&function).copied())
            }
            _ => return None,
        };
        let role = ir
            .semantic_call_roles
            .get(&expression)
            .copied()
            .or_else(|| {
                callable.and_then(|callable| ir.callable_semantic_roles.get(&callable).copied())
            });
        (role == Some(SemanticCallRole::KotlinAnyToString)).then_some(receiver)
    }
}
