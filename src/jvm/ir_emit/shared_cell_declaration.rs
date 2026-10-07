//! A captured mutable local's `Ref$XxxRef` holder. A declaration takes kotlinc's
//! `SharedVariablesLowering` shape: the local stores a fresh holder, and a separate statement then
//! sets its `element` to the initial value, which a declaration without an initializer omits. The
//! holder's debug range opens at its store, before the element is set.
//!
//! kotlinc boxes only a local that a closure it does not inline captures; a lambda it inlines
//! writes the caller's local directly. krusty still boxes that local, and the captured-vars
//! pass turns the holder back into a plain local. The declaration stores the holder before
//! setting its element — the same shape as a closure that keeps the box. Once the pass removes
//! the box, the local's range opens on the real initializer and a default store precedes it.

use super::*;
use crate::ir::ExprId;

/// A holder's element type and the initial value its declaration sets, if any.
pub(super) type SharedCell = (Ty, Option<ExprId>);

impl Emitter<'_> {
    /// The holder a declaration's `RefNew` initializer creates, when the declaration stores it
    /// before setting its element. An initial value that cannot carry the loaded holder on the
    /// operand stack (see `spills_operand_prefix`) is set on the holder before the store instead.
    pub(super) fn stored_shared_cell(&self, initializer: ExprId) -> Option<SharedCell> {
        match *self.ir.expr(initializer) {
            IrExpr::RefNew { elem, init }
                if !init.is_some_and(|value| self.spills_operand_prefix(value)) =>
            {
                Some((elem, self.holder_initial_value(elem, init)))
            }
            _ => None,
        }
    }

    /// The initial value a new holder's `element` is set to. A constant equal to the field's JVM
    /// default (zero bits, `false`, `null`) is left unset, as kotlinc's `SharedVariablesManager`
    /// leaves it: the fresh holder already holds it. kotlinc stores an unsigned zero, whose
    /// constant it does not read as a default.
    pub(super) fn holder_initial_value(&self, elem: Ty, init: Option<ExprId>) -> Option<ExprId> {
        use crate::ir::IrConst;
        let value = init?;
        let IrExpr::Const(constant) = self.ir.expr(value) else {
            return Some(value);
        };
        // An `ObjectRef` holds a reference, whose default is `null` whatever value it boxes.
        if ref_class(&elem).1 == "Ljava/lang/Object;" {
            return (!matches!(constant, IrConst::Null)).then_some(value);
        }
        let default = match constant {
            IrConst::Boolean(value) => !value,
            IrConst::Byte(value) => *value == 0,
            IrConst::Short(value) => *value == 0,
            IrConst::Int(value) => *value == 0,
            IrConst::Long(value) => *value == 0,
            IrConst::Float(value) => value.to_bits() == 0,
            IrConst::Double(value) => value.to_bits() == 0,
            IrConst::Char(value) => *value == 0,
            _ => false,
        };
        (!default).then_some(value)
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
        if let Some(&line) = self.ir.expr_lines.get(&declaration) {
            self.mark_expression_line(declaration, line, code);
        }
        let holder = Ty::obj(class);
        store(holder, slot, code);
        self.mark_suspend_lambda_parameter_read(declaration, code);
        self.open_declared_local(declaration, slot, holder, code);
        if let Some(value) = value {
            // The element store is the declaration's statement: its value marks its own line,
            // and the `putfield` returns to the declaration's.
            load(holder, slot, code);
            self.mark_expression_start(value, code);
            self.emit_element_value(elem, value, code);
            if let Some(&line) = self.ir.expr_lines.get(&declaration) {
                self.mark_expression_line(declaration, line, code);
            }
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
        let Some(init) = self.holder_initial_value(elem, init) else {
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
            self.emit_element_value(elem, init, code);
        }
        self.put_element(&elem, code);
    }

    /// Emit a value stored into a holder's `element`. An `ObjectRef` stores `Object`, so a
    /// suspension point's erased result is stored as it is, without the narrowing kotlinc writes
    /// only for a consumer that needs it.
    pub(super) fn emit_element_value(&mut self, elem: Ty, value: ExprId, code: &mut CodeBuilder) {
        let object = Ty::obj("java/lang/Object");
        if ir_ty_to_jvm(&elem).is_reference()
            && self.emit_erased_suspension_result(value, object, code)
        {
            return;
        }
        self.emit_value(value, code);
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
    ) -> Option<u64> {
        let Some(name) = self
            .record_locals
            .then(|| super::super::debug_local_names::declared_name(self.ir, declaration))
            .flatten()
        else {
            return None;
        };
        if code.bytes.len() > u16::MAX as usize {
            return None;
        }
        let provenance = self.ir.debug_local_provenance(declaration);
        let marker = provenance.is_some_and(crate::ir::IrDebugLocalProvenance::is_inline_marker);
        let precedes_recorded_locals = matches!(
            provenance,
            Some(crate::ir::IrDebugLocalProvenance::LambdaFrameMarker { .. })
        );
        let inline_operand = match provenance {
            Some(crate::ir::IrDebugLocalProvenance::InlineValue { .. }) => {
                self.ir.call_operand_bindings.contains(&declaration)
            }
            Some(crate::ir::IrDebugLocalProvenance::InlineLambdaReceiver { .. })
            | Some(crate::ir::IrDebugLocalProvenance::InlineLambdaParameter { .. })
            | Some(crate::ir::IrDebugLocalProvenance::InlineCallableReferenceParameter {
                ..
            }) => true,
            _ => false,
        };
        let inline_frame = if marker {
            let frame = self.next_inline_frame_identity;
            self.next_inline_frame_identity = self.next_inline_frame_identity.wrapping_add(1);
            Some(frame)
        } else if inline_operand {
            None
        } else {
            self.active_inline_frame_identity()
        };
        // The LocalVariableTable retains an inline parameter declaration's erased type. Keep that
        // debug representation independent of whether the verifier slot uses the erased bound, a
        // call-site primitive, or a call-site reference for an erased-top parameter.
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
            inline_operand,
            inline_frame,
            opens_inline_frame: marker,
            precedes_recorded_locals,
            explicit_end: None,
        };
        // kotlinc lists an inline frame's marker ahead of the locals binding that frame's operands,
        // which are declared before it in the same block: the inlined body's own table puts its
        // parameters last.
        let position = if marker {
            self.open_locals
                .iter()
                .position(|open| open.depth >= self.block_depth)
                .unwrap_or(self.open_locals.len())
        } else {
            self.open_locals.len()
        };
        self.open_locals.insert(position, local);
        inline_frame
    }
}
