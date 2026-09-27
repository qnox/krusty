//! Operand sequencing: evaluation order, frame-safe spilling across branchy operands, and the
//! representation adapter each consumer applies as an operand lands.

use super::*;

/// Whether a sequence's consumer materializes each operand at its own slot type.
#[derive(Clone, Copy)]
enum OperandUse {
    /// The adapter coerces to the consumer's slot, so an erased generic result stays erased.
    Materialized,
    /// No adapter: each operand arrives at its own checked type.
    AsEmitted,
}

impl Emitter<'_> {
    /// Push `ops` onto the stack in order. If any later op introduces control flow, evaluate all ops
    /// into temps first, then load them, keeping the operand baseline empty across nested branches.
    pub(super) fn emit_operands(&mut self, ops: &[u32], code: &mut CodeBuilder) {
        self.sequence_operands(None, ops, code, OperandUse::AsEmitted, |_, _, _| {});
    }

    /// Frame-safe operand sequencing with one representation adapter applied immediately after each
    /// value is pushed. Keeping the adapter inside the shared spill/load loop is essential for
    /// category-changing bridges such as primitive boxing: a wide left operand cannot be repaired
    /// after a right operand has landed above it, and a branchy right operand still requires both
    /// source expressions to be evaluated with an empty stack. Consumers supply only the boundary
    /// adapter; evaluation order, frame safety, temporary ownership, and cleanup remain centralized.
    ///
    /// The adapter materializes each operand at its consumer's slot, so a generic call's erased
    /// result reaches it unnarrowed (see [`Self::emit_consumed_operand`]).
    pub(super) fn emit_operands_adapted<F>(
        &mut self,
        default_plan: Option<(
            u32,
            &[crate::jvm::default_call_operands::DefaultOperandOrigin],
        )>,
        ops: &[u32],
        code: &mut CodeBuilder,
        adapt: F,
    ) where
        F: FnMut(&mut Self, Ty, &mut CodeBuilder),
    {
        self.sequence_operands(default_plan, ops, code, OperandUse::Materialized, adapt);
    }

    fn sequence_operands<F>(
        &mut self,
        default_plan: Option<(
            u32,
            &[crate::jvm::default_call_operands::DefaultOperandOrigin],
        )>,
        ops: &[u32],
        code: &mut CodeBuilder,
        operand_use: OperandUse,
        mut adapt: F,
    ) where
        F: FnMut(&mut Self, Ty, &mut CodeBuilder),
    {
        let mut inside_run = false;
        if ops.iter().skip(1).any(|&o| self.emits_control_flow(o)) {
            let temps = ops
                .iter()
                .map(|&o| {
                    let source = self.emit_sequenced_operand(o, operand_use, code);
                    self.spill_operand(o, source, code)
                })
                .collect::<Vec<_>>();
            for (operand_index, (&(slot, t, _), _)) in temps.iter().zip(ops).enumerate() {
                self.mark_synthesized_operand_run(
                    default_plan,
                    operand_index,
                    &mut inside_run,
                    code,
                );
                load(t, slot, code);
                adapt(self, t, code);
            }
            self.release_operand_spills(&temps);
        } else {
            for (operand_index, &o) in ops.iter().enumerate() {
                self.mark_synthesized_operand_run(
                    default_plan,
                    operand_index,
                    &mut inside_run,
                    code,
                );
                let source = self.emit_sequenced_operand(o, operand_use, code);
                adapt(self, source, code);
            }
        }
    }

    /// Emit one operand of a sequence, answering the stack type its adapter or spill receives.
    fn emit_sequenced_operand(
        &mut self,
        o: u32,
        operand_use: OperandUse,
        code: &mut CodeBuilder,
    ) -> Ty {
        match operand_use {
            OperandUse::Materialized => self.emit_consumed_operand(o, code),
            OperandUse::AsEmitted => {
                self.emit_value(o, code);
                self.value_ty(o)
            }
        }
    }

    /// Adapter for an operand that must occupy an erased/reference comparison slot. Reference values
    /// are already in the required representation; [`box_prim_free`] changes only JVM scalars.
    pub(super) fn box_scalar_operand(&mut self, ty: Ty, code: &mut CodeBuilder) {
        box_prim_free(self.cw, code, ty);
    }

    /// Push the two operands of a referential `===`/`!==` that compares object refs, BOXING whichever
    /// side is a primitive right where it lands — kotlinc's shape for a mixed pair (`aload_0; iload_1;
    /// Integer.valueOf; if_acmpne`), which it accepts with only an "identity equality … can be unstable
    /// because of implicit boxing" warning. Boxing has to happen per operand rather than once at the
    /// end: a `Long`/`Double` left operand occupies two stack words, so a boxed right operand cannot be
    /// swapped past it. The shared adapted-operand path owns evaluation order, frame-aware spilling,
    /// and temporary cleanup; identity supplies only the primitive-to-reference adapter.
    pub(super) fn emit_identity_operands(&mut self, lhs: u32, rhs: u32, code: &mut CodeBuilder) {
        self.emit_operands_adapted(None, &[lhs, rhs], code, Self::box_scalar_operand);
    }

    /// Preserve a completed left operand across a branchy right operand. This is safe when
    /// the right side has no exception handler, suspension, or transfer to an enclosing loop.
    /// Ordinary inline splice branches preserve the prefix through final-body dataflow. This avoids
    /// artificial locals for JVM integer/long bit operations.
    pub(super) fn emit_binary_operands_with_live_prefix(
        &mut self,
        lhs: u32,
        rhs: u32,
        code: &mut CodeBuilder,
    ) {
        if self.emits_control_flow(rhs) && !self.must_spill_across(rhs) {
            self.emit_value(lhs, code);
            self.emit_value(rhs, code);
        } else {
            self.emit_operands(&[lhs, rhs], code);
        }
    }
}
