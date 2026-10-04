//! Body-local value typing while a checked lambda body is spliced into its caller.

use std::collections::{HashMap, HashSet};

use crate::ir::{IrExpr, IrFile};
use crate::jvm::classfile::CodeBuilder;
use crate::types::Ty;

use super::local_variable_representation::slot_type;
use super::Emitter;

/// Map each declaration index reachable inside one body to the JVM type of the slot it owns.
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
        // The slot the declaration owns, exactly as its emission enters it: a `Unit` declaration
        // is a `kotlin/Unit` reference, and reading it pushes one operand.
        if let IrExpr::Variable { index, ty, .. } = ir.expr(expression) {
            declarations.insert(*index, slot_type(ir, expression, *ty));
        }
        match ir.expr(expression) {
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
        self.in_inline_body_scope(inline_body, param_slots, |emitter| {
            let result = emitter.value_ty(inline_body);
            emitter.emit_value(inline_body, code);
            result
        })
    }

    /// The type a lambda body leaves, typed in the body's own scope as [`Self::emit_fn_body_inline`]
    /// emits it.
    pub(super) fn inline_body_result_ty(
        &mut self,
        inline_body: u32,
        param_slots: &[(u16, Ty)],
    ) -> Ty {
        self.in_inline_body_scope(inline_body, param_slots, |emitter| {
            emitter.value_ty(inline_body)
        })
    }

    fn in_inline_body_scope<R>(
        &mut self,
        inline_body: u32,
        param_slots: &[(u16, Ty)],
        within: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let saved_slots = std::mem::take(&mut self.slots);
        // The body numbers its values from zero again: none of them is a constructor-property
        // parameter of the class whose initializer hosts the call.
        let saved_initializer_class = self.constructor_initializer_class.take();
        let saved_var_types = std::mem::replace(
            &mut self.var_types,
            collect_body_var_types(self.ir, std::iter::once(inline_body)),
        );
        for (index, &(slot, ty)) in param_slots.iter().enumerate() {
            self.slots.insert(index as u32, (slot, ty));
        }
        let result = within(self);
        self.constructor_initializer_class = saved_initializer_class;
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

    #[test]
    fn a_unit_declaration_is_typed_as_the_reference_slot_it_owns() {
        let mut ir = IrFile::default();
        let unit = ir.add_expr(IrExpr::UnitInstance);
        let declaration = ir.add_expr(IrExpr::Variable {
            index: 0,
            ty: Ty::Unit,
            init: Some(unit),
            named: false,
        });

        assert_eq!(
            collect_body_var_types(&ir, [declaration]).get(&0),
            Some(&Ty::obj("kotlin/Unit")),
            "a read of the declaration pushes the reference its slot holds"
        );
    }

    #[test]
    fn declarations_inside_a_variable_initializer_stay_in_the_current_body() {
        let mut ir = IrFile::default();
        let inner = ir.add_expr(IrExpr::Variable {
            index: 1,
            ty: Ty::String,
            init: None,
            named: false,
        });
        let initializer = ir.add_expr(IrExpr::Block {
            stmts: vec![inner],
            value: None,
        });
        let outer = ir.add_expr(IrExpr::Variable {
            index: 0,
            ty: Ty::Int,
            init: Some(initializer),
            named: false,
        });

        assert_eq!(
            collect_body_var_types(&ir, [outer]),
            HashMap::from([(0, Ty::Int), (1, Ty::String)])
        );
    }
}
