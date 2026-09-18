//! The call-operand/descriptor contract: pushing a selected call's operands in the exact shape its
//! physical JVM descriptor declares.
//!
//! A selected, type-checked call whose argument vector disagrees with the descriptor it was realized
//! against is a defect upstream of emission — a provider normalization or checked-call realization
//! that rewrote one side without the other. Emission cannot repair it and must not guess: choosing
//! either side would write a method whose operand stack does not match its own invocation.
//!
//! So the contract is a typed refusal. Every entry point checks arity BEFORE pushing anything and
//! returns [`DescriptorArityMismatch`] instead, leaving the operand stack untouched; each caller is
//! then obliged to abandon the whole call — never to emit the `invoke*` that would follow — and to
//! leave its own arm stack-correct via [`Emitter::bail_descriptor_arity`].

use super::*;

/// A selected call whose checked argument vector disagrees with its physical descriptor.
///
/// Carried rather than reported at the point of detection: the operand helpers do not know whether
/// they are inside a speculative inline splice (where the right answer is to abandon the splice and
/// let the real call stand) or on the committed emission path.
#[derive(Debug, Clone)]
pub(super) struct DescriptorArityMismatch {
    /// The owning class, when the site knows it. Absent for descriptor-only operand pushes.
    pub(super) owner: Option<String>,
    /// Operands the checked call supplied.
    pub(super) supplied: usize,
    /// Parameters the physical descriptor declares.
    pub(super) physical: usize,
}

impl DescriptorArityMismatch {
    fn check(owner: Option<&str>, supplied: usize, physical: usize) -> Result<(), Self> {
        if supplied == physical {
            return Ok(());
        }
        Err(Self {
            owner: owner.map(str::to_string),
            supplied,
            physical,
        })
    }
}

impl Emitter<'_> {
    /// Where a non-virtual call to an interface member must go under `-jvm-default=disable`.
    ///
    /// `super.f()` and a call to a private interface member both push the receiver first and then
    /// `invokespecial` the interface. Under `disable` the interface holds no body, so the call has to
    /// become `invokestatic <Iface>$DefaultImpls.f(LIface;…)` — the receiver already on the stack is
    /// exactly the holder static's parameter 0. Returns `None` when the call should stay as it is.
    pub(super) fn holder_call(
        &self,
        owner: &str,
        descriptor: &str,
        current_source_body: bool,
    ) -> Option<(String, String)> {
        if self.jvm_default != JvmDefaultMode::Disable || !current_source_body {
            return None;
        }
        let holder_descriptor = descriptor
            .strip_prefix('(')
            .map(|rest| format!("(L{owner};{rest}"))?;
        Some((format!("{owner}$DefaultImpls"), holder_descriptor))
    }

    /// Abandon a call whose operands cannot be pushed, leaving the current arm stack-correct.
    ///
    /// The caller must `return` immediately afterwards without emitting its `invoke*`. `ret` is the
    /// value the abandoned arm was going to leave behind: a typed zero stands in for it so the now
    /// unreachable code that follows still assembles and the class still passes the verifier's
    /// structural checks before the file is skipped.
    pub(super) fn bail_descriptor_arity(
        &mut self,
        mismatch: &DescriptorArityMismatch,
        ret: Ty,
        code: &mut CodeBuilder,
    ) {
        crate::trace_compiler!(
            "emit",
            "descriptor arity mismatch{} ({} operands vs {} params)",
            mismatch
                .owner
                .as_deref()
                .map(|owner| format!(" for {owner}"))
                .unwrap_or_default(),
            mismatch.supplied,
            mismatch.physical,
        );
        self.run.set_inline_bail("call arity mismatch");
        if ret != Ty::Unit {
            push_zero(ret, code, self.cw);
        }
    }
}

impl Emitter<'_> {
    pub(super) fn emit_descriptor_operands(
        &mut self,
        ops: &[u32],
        physical: &[Ty],
        code: &mut CodeBuilder,
    ) -> Result<(), DescriptorArityMismatch> {
        DescriptorArityMismatch::check(None, ops.len(), physical.len())?;
        // Fail CLOSED, exactly as the sibling `MethodCall` path does for the same condition: record
        // the stable reason, stop emitting, and let the backend report
        // `JVM backend inline error: call arity mismatch`. Emitting an unverifiable call is worse,
        // and so is panicking — that loses the diagnostic and takes down the whole compilation
        let mut index = 0usize;
        self.emit_operands_adapted(ops, code, |this, source, code| {
            let expression = ops[index];
            let target = physical[index];
            index += 1;
            this.adapt_physical_operand_for(expression, source, target, code);
        });
        Ok(())
    }

    pub(super) fn emit_call_descriptor_operands(
        &mut self,
        call_expression: u32,
        ops: &[u32],
        physical: &[Ty],
        code: &mut CodeBuilder,
    ) -> Result<(), DescriptorArityMismatch> {
        DescriptorArityMismatch::check(None, ops.len(), physical.len())?;
        let mut index = 0usize;
        self.emit_operands_adapted(ops, code, |this, source, code| {
            let parameter_index = index;
            let expression = ops[parameter_index];
            let target = physical[parameter_index];
            index += 1;
            this.adapt_physical_call_operand_for(
                call_expression,
                parameter_index,
                expression,
                source,
                target,
                code,
            );
        });
        Ok(())
    }

    pub(super) fn emit_descriptor_virtual_operands(
        &mut self,
        call_expression: u32,
        owner: &str,
        receiver: u32,
        args: &[u32],
        physical_params: &[Ty],
        code: &mut CodeBuilder,
    ) -> Result<(), DescriptorArityMismatch> {
        DescriptorArityMismatch::check(Some(owner), args.len(), physical_params.len())?;
        let mut ops = Vec::with_capacity(args.len() + 1);
        ops.push(receiver);
        ops.extend(args.iter().copied());
        let mut physical = Vec::with_capacity(physical_params.len() + 1);
        let owner_ty = Ty::obj(owner);
        physical.push(if owner_ty.scalar_value_repr().is_some() {
            Ty::nullable(owner_ty)
        } else {
            owner_ty
        });
        physical.extend_from_slice(physical_params);
        let mut index = 0usize;
        self.emit_operands_adapted(&ops, code, |this, source, code| {
            let operand = ops[index];
            let target = physical[index];
            if index == 0 {
                this.adapt_physical_operand_for(operand, source, target, code);
            } else {
                this.adapt_physical_call_operand_for(
                    call_expression,
                    index - 1,
                    operand,
                    source,
                    target,
                    code,
                );
            }
            index += 1;
        });
        Ok(())
    }
}

/// The contract's own refusals, one per callee form that reaches it.
///
/// Reaching a mismatch needs malformed IR — a valid Kotlin source cannot produce one — so these
/// build the IR directly. Each asserts BOTH halves of the contract: the stable bail category, and
/// that no class file is produced, which is what proves the `invoke*` was abandoned rather than
/// emitted against operands that were never pushed.
#[cfg(test)]
mod tests {
    use crate::ir::{Callee, IrConst, IrExpr, IrFile, IrFunction};
    use crate::jvm::ir_emit::fail_soft_tests::emit_for_test;
    use crate::jvm::ir_emit::EmitRun;
    use crate::types::Ty;

    #[test]
    fn arity_failure_exposes_category_without_owner_or_callable_name() {
        let mut ir = IrFile::default();
        let unit = ir.add_expr(IrExpr::Block {
            stmts: vec![],
            value: None,
        });
        let callee = ir.add_fun(IrFunction {
            name: "realCallableName".into(),
            params: vec![Ty::Int],
            ret: Ty::Unit,
            body: Some(unit),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![],
        });
        let mismatched_call = ir.add_expr(IrExpr::Call {
            callee: Callee::Local(callee),
            dispatch_receiver: None,
            args: vec![],
        });
        ir.add_fun(IrFunction {
            name: "box".into(),
            params: vec![],
            ret: Ty::Unit,
            body: Some(mismatched_call),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![],
        });

        // The trace may identify `SensitiveFacade.realCallableName`, but the result read by the CLI
        // and survey is deliberately a stable category with neither source nor JVM owner spelling.
        let run = EmitRun::default();
        assert!(emit_for_test(&ir, "SensitiveFacade", &run).is_none());
        assert_eq!(run.inline_bail().as_deref(), Some("call arity mismatch"));
    }

    /// Emit one `box()` whose only statement is `call`, and report what the backend did with it.
    fn emit_single_call(ir: &mut IrFile, call: u32) -> (bool, Option<String>) {
        ir.add_fun(IrFunction {
            name: "box".into(),
            params: vec![],
            ret: Ty::Unit,
            body: Some(call),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![],
        });
        let run = EmitRun::default();
        let emitted = emit_for_test(ir, "Facade", &run).is_some();
        (emitted, run.inline_bail())
    }

    /// A CROSS-FILE top-level call whose argument vector is shorter than the parameters it was
    /// realized against. The operand contract refuses, so the file is skipped — the alternative is
    /// an `invokestatic` whose operand stack does not match its own descriptor.
    #[test]
    fn a_cross_file_call_short_of_its_parameters_is_refused_not_emitted() {
        let mut ir = IrFile::default();
        let call = ir.add_expr(IrExpr::Call {
            callee: Callee::CrossFile {
                facade: crate::types::type_name("other/OtherKt"),
                name: "takesTwo".into(),
                params: vec![Ty::Int, Ty::Int],
                ret: Ty::Unit,
                module_target: None,
                module_default_call: false,
            },
            dispatch_receiver: None,
            args: vec![],
        });
        let (emitted, bail) = emit_single_call(&mut ir, call);
        assert!(!emitted, "a refused call must not produce a class file");
        assert_eq!(bail.as_deref(), Some("call arity mismatch"));
    }
    /// The same for a resolved classpath STATIC, which carries a verbatim JVM descriptor rather
    /// than semantic parameter types.
    #[test]
    fn a_classpath_static_call_short_of_its_descriptor_is_refused_not_emitted() {
        let mut ir = IrFile::default();
        let call = ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: crate::types::type_name("kotlin/text/StringsKt"),
                name: "takesTwo".into(),
                descriptor: "(Ljava/lang/String;I)V".into(),
                inline: crate::libraries::InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![],
        });
        let (emitted, bail) = emit_single_call(&mut ir, call);
        assert!(!emitted, "a refused call must not produce a class file");
        assert_eq!(bail.as_deref(), Some("call arity mismatch"));
    }

    /// And for a VIRTUAL call, where the receiver is pushed before the arguments: a refusal must
    /// happen before anything reaches the operand stack.
    #[test]
    fn a_virtual_call_short_of_its_descriptor_is_refused_not_emitted() {
        let mut ir = IrFile::default();
        let receiver = ir.add_expr(IrExpr::Const(IrConst::Null));
        let call = ir.add_expr(IrExpr::Call {
            callee: Callee::Virtual {
                owner: crate::types::type_name("java/lang/String"),
                name: "takesTwo".into(),
                descriptor: "(Ljava/lang/String;I)V".into(),
                params: None,
                interface: false,
            },
            dispatch_receiver: Some(receiver),
            args: vec![],
        });
        let (emitted, bail) = emit_single_call(&mut ir, call);
        assert!(!emitted, "a refused call must not produce a class file");
        assert_eq!(bail.as_deref(), Some("call arity mismatch"));
    }
}
