//! Body-local value typing while a checked lambda body is spliced into its caller.

use std::collections::{HashMap, HashSet};

use crate::ir::{IrExpr, IrFile};
use crate::jvm::classfile::CodeBuilder;
use crate::types::Ty;

use super::{ir_ty_to_jvm, Emitter};

/// Map each declaration index reachable inside one body to its physical JVM type.
///
/// Value indices restart for every body. A lambda construction belongs to the current body through
/// its capture expressions, but its `inline_body` is a separate numbering domain and must not be
/// traversed here. The generic IR child walk deliberately includes that body for whole-tree passes,
/// so this ownership boundary has to be explicit.
pub(super) fn collect_body_var_types(
    ir: &IrFile,
    roots: impl IntoIterator<Item = u32>,
) -> HashMap<u32, Ty> {
    let mut declarations = HashMap::new();
    let mut seen = HashSet::new();
    let mut pending = roots.into_iter().collect::<Vec<_>>();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        match ir.expr(expression) {
            IrExpr::Variable { index, ty, .. } => {
                declarations.insert(*index, ir_ty_to_jvm(ty));
            }
            IrExpr::Lambda { captures, .. } => pending.extend(captures.iter().copied()),
            _ => crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child)),
        }
    }
    declarations
}

impl Emitter<'_> {
    /// Emit a lambda body inline at a stdlib-inline call site with its own slots and declaration
    /// types. A source return in this body remains a real return from the enclosing method.
    pub(super) fn emit_fn_body_inline(
        &mut self,
        inline_body: u32,
        param_slots: &[(u16, Ty)],
        code: &mut CodeBuilder,
    ) -> Ty {
        let saved_slots = std::mem::take(&mut self.slots);
        let saved_var_types = std::mem::replace(
            &mut self.var_types,
            collect_body_var_types(self.ir, std::iter::once(inline_body)),
        );
        for (index, &(slot, ty)) in param_slots.iter().enumerate() {
            self.slots.insert(index as u32, (slot, ty));
        }
        let result = self.value_ty(inline_body);
        self.emit_value(inline_body, code);
        self.slots = saved_slots;
        self.var_types = saved_var_types;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nested_inline_body_cannot_overwrite_its_parents_same_index() {
        let mut ir = IrFile::default();
        let parent = ir.add_expr(IrExpr::Variable {
            index: 0,
            ty: Ty::UInt,
            init: None,
            named: false,
        });
        let nested = ir.add_expr(IrExpr::Variable {
            index: 0,
            ty: Ty::String,
            init: None,
            named: false,
        });
        let nested_body = ir.add_expr(IrExpr::Block {
            stmts: vec![nested],
            value: None,
        });
        let lambda = ir.add_expr(IrExpr::Lambda {
            impl_fn: 0,
            arity: 0,
            captures: Vec::new(),
            sam: None,
            inline_body: Some(nested_body),
        });
        let parent_body = ir.add_expr(IrExpr::Block {
            stmts: vec![parent, lambda],
            value: None,
        });

        assert_eq!(
            collect_body_var_types(&ir, [parent_body]).get(&0),
            Some(&Ty::Int),
            "the parent keeps UInt's physical Int carrier"
        );
        assert_eq!(
            collect_body_var_types(&ir, [nested_body]).get(&0),
            Some(&Ty::String),
            "the nested body owns its same-numbered declaration"
        );
    }
}
