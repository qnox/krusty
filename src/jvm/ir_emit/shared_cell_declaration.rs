//! A captured mutable local's `Ref$XxxRef` holder. A declaration takes kotlinc's
//! `SharedVariablesLowering` shape: the local stores a fresh holder, and a separate statement then
//! sets its `element` to the initial value. A declaration without an initializer, or whose
//! initializer is the element's default (`null`, `false`, a zero), omits that statement. The
//! holder's debug range opens at its store, before the element is set.
//!
//! kotlinc boxes only a local that a closure it does not inline captures; a lambda it inlines
//! writes the caller's local directly. krusty boxes a local its inlined lambdas capture too, and
//! the captured-vars optimization then turns the holder back into a plain local. Such a holder
//! sets its element before its store, so the local it becomes is assigned before its range
//! begins, as kotlinc's plain local is.

use super::*;
use crate::ir::ExprId;

/// A holder's element type and the initial value its declaration sets, if any.
pub(super) type SharedCell = (Ty, Option<ExprId>);

impl Emitter<'_> {
    /// The holder a declaration's `RefNew` initializer creates, when the declaration stores it
    /// before setting its element. An initial value that cannot carry the loaded holder on the
    /// operand stack (see `spills_operand_prefix`) is set on the holder before the store instead.
    pub(super) fn stored_shared_cell(
        &self,
        declaration: ExprId,
        initializer: ExprId,
    ) -> Option<SharedCell> {
        match *self.ir.expr(initializer) {
            IrExpr::RefNew { elem, init }
                if !init.is_some_and(|value| self.spills_operand_prefix(value))
                    && !self.inlined_only_cells.contains(&declaration) =>
            {
                Some((elem, init))
            }
            _ => None,
        }
    }

    /// Record which of a block's captured-local declarations only inlined lambdas capture: every
    /// later use of the local in the block reads or writes the holder's element, or is a capture
    /// of a lambda with an inlined body.
    pub(super) fn note_inlined_only_cells(&mut self, stmts: &[ExprId], value: Option<ExprId>) {
        for (position, &statement) in stmts.iter().enumerate() {
            let IrExpr::Variable {
                index,
                init: Some(initializer),
                ..
            } = *self.ir.expr(statement)
            else {
                continue;
            };
            if !matches!(self.ir.expr(initializer), IrExpr::RefNew { .. }) {
                continue;
            }
            let scope = stmts[position + 1..].iter().copied().chain(value);
            if scope
                .into_iter()
                .all(|root| only_inlined_captures(self.ir, root, index))
            {
                self.inlined_only_cells.insert(statement);
            }
        }
    }

    /// Store a new holder in `slot`, then set its `element` to the initial value, if any.
    pub(super) fn emit_shared_cell_declaration(
        &mut self,
        declaration: ExprId,
        slot: u16,
        (elem, value): SharedCell,
        code: &mut CodeBuilder,
    ) {
        let (class, _) = ref_class(&elem);
        self.emit_new_holder(class, code);
        debug_lines::mark_statement(self.ir, declaration, code);
        let holder = Ty::obj(class);
        store(holder, slot, code);
        self.mark_suspend_lambda_parameter_read(declaration, code);
        self.open_declared_local(declaration, slot, holder, code);
        if let Some(value) = value.filter(|&value| !holds_default_value(self.ir, elem, value)) {
            // The element store is the declaration's statement: its value marks its own line,
            // and the `putfield` returns to the declaration's.
            load(holder, slot, code);
            self.mark_expression_start(value, code);
            self.emit_value(value, code);
            debug_lines::mark_statement(self.ir, declaration, code);
            self.put_element(&elem, code);
        }
    }

    /// Leave a new holder with its element set to `init`, if any, on the operand stack. An `init`
    /// that cannot carry the operand stack is spilled first, so it runs with nothing of the
    /// holder's on the stack; other branching values run with the holder in place, as kotlinc's do.
    pub(super) fn emit_shared_cell(
        &mut self,
        elem: Ty,
        init: Option<ExprId>,
        code: &mut CodeBuilder,
    ) {
        let (class, _) = ref_class(&elem);
        let Some(init) = init else {
            self.emit_new_holder(class, code);
            return;
        };
        if self.spills_operand_prefix(init) {
            let temps = self.spill_to_temps(&[init], code);
            self.emit_new_holder(class, code);
            code.dup();
            for &(slot, ty, _) in &temps {
                load(ty, slot, code);
            }
            self.release_operand_spills(&temps);
        } else {
            self.emit_new_holder(class, code);
            code.dup();
            self.emit_value(init, code);
        }
        self.put_element(&elem, code);
    }

    fn emit_new_holder(&mut self, class: &str, code: &mut CodeBuilder) {
        let class_ref = self.cw.class_ref(class);
        code.new_obj(class_ref);
        code.dup();
        let constructor = self.cw.methodref(class, "<init>", "()V");
        code.invokespecial(constructor, 0, 0);
    }

    fn put_element(&mut self, elem: &Ty, code: &mut CodeBuilder) {
        let (class, descriptor) = ref_class(elem);
        let element = self.cw.fieldref(class, "element", descriptor);
        code.putfield(element, slot_words(ir_ty_to_jvm(elem)) as i32);
    }

    /// Open a source local's debug range at the current offset, once its slot holds a value (or,
    /// for an uninitialized declaration, at its declaration).
    pub(super) fn open_declared_local(
        &mut self,
        declaration: ExprId,
        slot: u16,
        ty: Ty,
        code: &CodeBuilder,
    ) {
        let Some(name) = self
            .record_locals
            .then(|| super::super::debug_local_names::declared_name(self.ir, declaration))
            .flatten()
        else {
            return;
        };
        if code.bytes.len() > u16::MAX as usize {
            return;
        }
        let provenance = self.ir.debug_local_provenance(declaration);
        // A spliced lambda's marker precedes, in kotlinc's table, every local its body declares.
        let table_position = matches!(
            provenance,
            Some(crate::ir::IrDebugLocalProvenance::LambdaFrameMarker { .. })
        )
        .then(|| code.local_entry_count());
        // A materialized inline parameter executes in a call-site-specialized slot, but kotlinc's
        // LocalVariableTable retains the parameter declaration's erased type. Keep that debug
        // representation separate from the verifier/storage type above.
        let debug_ty = self
            .ir
            .inline_operand_declared_type(declaration)
            .map(|declared| ir_ty_to_jvm(&stored_value_ty(declared)))
            .unwrap_or(ty);
        let local = super::block_scope::OpenLocal {
            depth: self.block_depth,
            slot,
            start: code.bytes.len() as u16,
            name,
            descriptor: local_variable_desc(debug_ty),
            inline_operand: self.ir.call_operand_bindings.contains(&declaration)
                && matches!(
                    provenance,
                    Some(crate::ir::IrDebugLocalProvenance::InlineValue { .. })
                        | Some(crate::ir::IrDebugLocalProvenance::InlineLambdaReceiver { .. })
                ),
            table_position,
            explicit_end: None,
        };
        // kotlinc lists an inline frame's marker ahead of the locals binding that frame's operands,
        // which are declared before it in the same block: the inlined body's own table puts its
        // parameters last.
        let marker = provenance.is_some_and(crate::ir::IrDebugLocalProvenance::is_inline_marker);
        let position = if marker {
            self.open_locals
                .iter()
                .position(|open| open.depth >= self.block_depth)
                .unwrap_or(self.open_locals.len())
        } else {
            self.open_locals.len()
        };
        self.open_locals.insert(position, local);
    }
}

/// Whether `value` is the constant a fresh holder's `element` already holds: `null`, `false` or a
/// zero of the element's own kind. kotlinc sets no element for such an initializer; `-0.0` is not
/// one, since its bits differ from the field's default.
fn holds_default_value(ir: &crate::ir::IrFile, elem: Ty, value: ExprId) -> bool {
    use crate::ir::IrConst;
    let IrExpr::Const(constant) = ir.expr(value) else {
        return false;
    };
    match constant {
        IrConst::Null => !elem.is_jvm_scalar(),
        IrConst::Boolean(value) => elem.is_jvm_scalar() && !*value,
        IrConst::Byte(value) => elem.is_jvm_scalar() && *value == 0,
        IrConst::Short(value) => elem.is_jvm_scalar() && *value == 0,
        IrConst::Int(value) => elem.is_jvm_scalar() && *value == 0,
        IrConst::Long(value) => elem.is_jvm_scalar() && *value == 0,
        IrConst::Char(value) => elem.is_jvm_scalar() && *value == 0,
        IrConst::UByte(value) => elem.is_jvm_scalar() && *value == 0,
        IrConst::UShort(value) => elem.is_jvm_scalar() && *value == 0,
        IrConst::UInt(value) => elem.is_jvm_scalar() && *value == 0,
        IrConst::ULong(value) => elem.is_jvm_scalar() && *value == 0,
        IrConst::Float(value) => elem.is_jvm_scalar() && value.to_bits() == 0,
        IrConst::Double(value) => elem.is_jvm_scalar() && value.to_bits() == 0,
        IrConst::String(_) => false,
    }
}

/// Whether every use of local `index` under `root` reads or writes its holder's element, or is a
/// capture of a lambda an inline call expands: a literal argument of a call with published inline
/// parameter modifiers, for a parameter that is not `noinline`. Such a lambda's body is spliced
/// into the caller, so the search follows the holder into it: the body numbers its captures first,
/// and the capture at position `k` is that body's value `k`. Any other lambda's body is its own
/// function, so only its captures are searched, and capturing the holder there lets it escape.
fn only_inlined_captures(ir: &crate::ir::IrFile, root: ExprId, index: u32) -> bool {
    let is_local = |expression: ExprId, local: u32| matches!(*ir.expr(expression), IrExpr::GetValue(read) if read == local);
    let mut pending = vec![(root, index)];
    while let Some((expression, local)) = pending.pop() {
        if is_local(expression, local) {
            return false;
        }
        match ir.expr(expression) {
            IrExpr::RefGet { holder, .. } if is_local(*holder, local) => {}
            IrExpr::RefSet { holder, value, .. } if is_local(*holder, local) => {
                pending.push((*value, local))
            }
            IrExpr::Call {
                dispatch_receiver,
                args,
                ..
            } if ir.call_inline_modifiers.contains_key(&expression) => {
                pending.extend(dispatch_receiver.map(|receiver| (receiver, local)));
                let modifiers = &ir.call_inline_modifiers[&expression];
                for (position, &argument) in args.iter().enumerate() {
                    match ir.expr(argument) {
                        IrExpr::Lambda {
                            captures,
                            inline_body: Some(body),
                            ..
                        } if modifiers.get(position).is_some_and(|modifier| {
                            *modifier != crate::types::InlineParameterModifier::Noinline
                        }) =>
                        {
                            for (slot, &capture) in captures.iter().enumerate() {
                                if is_local(capture, local) {
                                    pending.push((*body, slot as u32));
                                } else {
                                    pending.push((capture, local));
                                }
                            }
                        }
                        _ => pending.push((argument, local)),
                    }
                }
            }
            IrExpr::Lambda { captures, .. } => {
                pending.extend(captures.iter().map(|&capture| (capture, local)))
            }
            _ => crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
                pending.push((child, local))
            }),
        }
    }
    true
}
