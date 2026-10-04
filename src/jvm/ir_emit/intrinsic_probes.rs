//! The debug-probe check kotlinc writes after a `suspendCoroutineUninterceptedOrReturn` block.
//!
//! The block's `Any?` result is `COROUTINE_SUSPENDED` when it suspended, and kotlinc then reports
//! the suspension to the debug probes with the continuation the block was given, leaving the
//! result on the stack: `dup; getCOROUTINE_SUSPENDED; if_acmpne L; <continuation>;
//! probeCoroutineSuspended; L:`, written at the call's own line.

use super::{load, Emitter};
use crate::ir::ExprId;
use crate::jvm::classfile::CodeBuilder;
use crate::types::Ty;

const CONTINUATION: &str = "kotlin/coroutines/Continuation";

impl Emitter<'_> {
    /// Follow `point`, when it is an unintercepted intrinsic suspension point, with its probe.
    pub(super) fn probe_intrinsic_suspension(&mut self, point: ExprId, code: &mut CodeBuilder) {
        if !self.ir.is_unintercepted_suspension(point) {
            return;
        }
        // A block the transformer takes probes the fake continuation it was given.
        let declared = match self.is_transformed_block(point) {
            true => None,
            false => {
                let Some(&continuation) = self.intrinsic_probe_continuations.get(&point) else {
                    self.run.set_emit_error(
                        "an unintercepted suspension point has no CPS continuation binding"
                            .to_string(),
                    );
                    return;
                };
                let Some(&declared) = self.slots.get(&continuation) else {
                    self.run.set_emit_error(
                        "a probed continuation has no declared value slot".to_string(),
                    );
                    return;
                };
                Some(declared)
            }
        };
        // The block ran as an inlined body, after which kotlinc writes the call's line afresh.
        if let Some(&line) = self.ir.expr_source_lines.get(&point) {
            if line != 0 {
                // Closing the spliced lambda may already have put the call line on a trailing
                // `nop`. The probe's first real instruction shares that line, so do not split the
                // two into separate debug ranges: kotlinc's redundant-nop pass then removes the
                // anchor. Without such an anchor the ordinary post-inline reset still forces the
                // call line to be written at the probe.
                if !code.line_is_anchored_by_trailing_nop(line) {
                    code.forget_line();
                    code.mark_line(line);
                }
            }
        }
        let resumed = code.new_label();
        code.dup();
        let suspended = self.cw.methodref(
            "kotlin/coroutines/intrinsics/IntrinsicsKt",
            "getCOROUTINE_SUSPENDED",
            "()Ljava/lang/Object;",
        );
        code.invokestatic(suspended, 0, 1);
        code.if_acmpne(resumed);
        match declared {
            Some((slot, ty)) => {
                load(ty, slot, code);
                // A machine's continuation is its own class; the probe takes the interface.
                if ty != Ty::obj(CONTINUATION) {
                    let interface = self.cw.class_ref(CONTINUATION);
                    code.checkcast(interface);
                }
            }
            None => self.load_fake_continuation(code),
        }
        let probe = self.cw.methodref(
            "kotlin/coroutines/jvm/internal/DebugProbesKt",
            "probeCoroutineSuspended",
            "(Lkotlin/coroutines/Continuation;)V",
        );
        code.invokestatic(probe, 1, 0);
        self.bind(resumed, code);
    }
}

#[cfg(test)]
mod tests {
    use crate::ir::{
        IrConst, IrExpr, IrFile, IrFunction, IrIntrinsicSuspensionPoint,
    };
    use crate::jvm::ir_emit::invariant_tests::emit_for_test_with_probe_continuations;
    use crate::jvm::ir_emit::EmitRun;
    use crate::types::Ty;

    #[test]
    fn an_unintercepted_point_without_a_cps_binding_fails_emission() {
        let mut ir = IrFile::default();
        let point = ir.add_expr(IrExpr::Const(IrConst::Null));
        ir.intrinsic_suspension_points.insert(
            point,
            IrIntrinsicSuspensionPoint {
                result: Ty::nullable(Ty::obj("kotlin/Any")),
            },
        );
        ir.add_fun(IrFunction {
            name: "point".into(),
            // Keep the fixture CPS-shaped while deliberately omitting only the authoritative
            // point-to-continuation binding under test.
            params: vec![Ty::obj(super::CONTINUATION)],
            ret: Ty::nullable(Ty::obj("kotlin/Any")),
            body: Some(point),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![None],
        });
        let continuations = crate::jvm::suspend::IntrinsicProbeContinuations::default();
        let run = EmitRun::default();

        assert!(
            emit_for_test_with_probe_continuations(&ir, "TestKt", &run, &continuations).is_none(),
            "an invalid probe fact must not produce a class file"
        );
        assert_eq!(
            run.emit_error().as_deref(),
            Some("an unintercepted suspension point has no CPS continuation binding"),
        );
    }
}
