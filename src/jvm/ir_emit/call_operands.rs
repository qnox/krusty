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

/// Map a physical descriptor operand back to the selected call's checked argument vector. A
/// retained receiver can lead the JVM operand list without belonging to that vector.
fn call_argument_index(
    physical_index: usize,
    leading_non_argument_operands: usize,
) -> Option<usize> {
    physical_index.checked_sub(leading_non_argument_operands)
}

/// A selected call whose checked argument vector disagrees with its physical descriptor.
///
/// Carried rather than reported at the point of detection: the operand helpers do not know whether
/// they are inside a speculative inline splice (where the right answer is to abandon the splice and
/// let the real call stand) or on the committed emission path.
#[derive(Debug, Clone)]
pub(super) struct DescriptorArityMismatch {
    /// The exact realized JVM callable, when the site knows it. Absent for descriptor-only operand
    /// pushes. The descriptor is part of the identity: owner and member alone do not distinguish
    /// overloads.
    pub(super) target: Option<JvmCallableIdentity>,
    /// Operands the checked call supplied.
    pub(super) supplied: usize,
    /// Parameters the physical descriptor declares.
    pub(super) physical: usize,
}

/// The already-realized JVM callable whose virtual operands are being pushed.
///
/// Callers pass this directly from the selected call representation. The operand contract never
/// looks a declaration up again or derives identity from a source spelling.
#[derive(Clone, Copy)]
pub(super) struct VirtualCallTarget<'a> {
    pub(super) owner: &'a str,
    pub(super) name: &'a str,
    pub(super) descriptor: &'a str,
}

#[derive(Debug, Clone)]
pub(super) struct JvmCallableIdentity {
    owner: String,
    name: String,
    descriptor: String,
}

impl std::fmt::Display for JvmCallableIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}{}", self.owner, self.name, self.descriptor)
    }
}

impl DescriptorArityMismatch {
    fn check(
        target: Option<VirtualCallTarget<'_>>,
        supplied: usize,
        physical: usize,
    ) -> Result<(), Self> {
        if supplied == physical {
            return Ok(());
        }
        Err(Self {
            target: target.map(|target| JvmCallableIdentity {
                owner: target.owner.to_string(),
                name: target.name.to_string(),
                descriptor: target.descriptor.to_string(),
            }),
            supplied,
            physical,
        })
    }
}

impl Emitter<'_> {
    fn default_operand_origins(
        &self,
        call: u32,
        operands: &[u32],
        required: bool,
    ) -> Vec<crate::jvm::default_call_operands::DefaultOperandOrigin> {
        if let Some(plan) = self.default_call_operands.matching(call, operands) {
            return plan.iter().map(|operand| operand.origin).collect();
        }
        if required || self.default_call_operands.contains(call) {
            self.run.set_emit_error(format!(
                "default call operand plan does not match expression {call}"
            ));
        }
        vec![crate::jvm::default_call_operands::DefaultOperandOrigin::Supplied; operands.len()]
    }

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
                .target
                .as_ref()
                .map(|target| format!(" for {target}"))
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
    /// Operands of a call to a function this module declares. Lowering already realized every
    /// representation change its checked arguments need, so each reference operand is only
    /// materialized at its parameter's type, the cast kotlinc writes for an upcast.
    pub(super) fn emit_source_call_operands(
        &mut self,
        call: u32,
        leading_non_argument_operands: usize,
        ops: &[u32],
        physical: &[Ty],
        code: &mut CodeBuilder,
    ) -> Result<(), DescriptorArityMismatch> {
        DescriptorArityMismatch::check(None, ops.len(), physical.len())?;
        let mut index = 0usize;
        self.emit_operands_adapted(None, ops, code, |this, source, code| {
            let parameter_index = index;
            let target = physical[index];
            index += 1;
            let is_continuation = parameter_index
                .checked_sub(leading_non_argument_operands)
                .is_some_and(|argument_index| {
                    this.default_call_operands
                        .is_continuation(call, argument_index)
                });
            if !is_continuation {
                this.coerce_reference_on_stack(source, target, code);
            }
        });
        Ok(())
    }

    /// The same representation boundary for a source `$default` stub. Unlike an ordinary source
    /// call, default realization has inserted placeholders, mask words, and the marker into `ops`;
    /// preserve their recorded line ownership while materializing every operand at the stub's
    /// already-realized parameter type.
    pub(super) fn emit_source_default_call_operands(
        &mut self,
        call: u32,
        ops: &[u32],
        physical: &[Ty],
        code: &mut CodeBuilder,
    ) -> Result<(), DescriptorArityMismatch> {
        DescriptorArityMismatch::check(None, ops.len(), physical.len())?;
        let origins = self.default_operand_origins(call, ops, true);
        let mut index = 0usize;
        self.emit_operands_adapted(Some((call, &origins)), ops, code, |this, source, code| {
            let parameter_index = index;
            let target = physical[index];
            index += 1;
            if !this
                .default_call_operands
                .is_continuation(call, parameter_index)
            {
                this.coerce_reference_on_stack(source, target, code);
            }
        });
        Ok(())
    }

    /// Put `call`'s line in effect if the operand at `position` opens a run of operands the CALL
    /// synthesized. The plan comes from the backend pass that realized them; nothing here decides
    /// from an operand's shape whether it was written or invented.
    pub(super) fn mark_synthesized_operand_run(
        &mut self,
        call: Option<(
            u32,
            &[crate::jvm::default_call_operands::DefaultOperandOrigin],
        )>,
        operand_index: usize,
        inside_run: &mut bool,
        code: &mut CodeBuilder,
    ) {
        let Some((call, origins)) = call else {
            return;
        };
        let synthesized = origins.get(operand_index).is_some_and(|origin| {
            *origin == crate::jvm::default_call_operands::DefaultOperandOrigin::Synthesized
        });
        self.mark_synthesized_run_start(call, synthesized, inside_run, code);
    }

    /// The same rule for a call whose synthesized operands emission realizes itself — a defaulted
    /// CONSTRUCTOR, whose placeholders, mask words and marker are pushed directly rather than
    /// entered into the operand vector as expressions.
    pub(super) fn mark_synthesized_run_start(
        &mut self,
        call: u32,
        synthesized: bool,
        inside_run: &mut bool,
        code: &mut CodeBuilder,
    ) {
        if synthesized && !*inside_run {
            self.mark_dispatch_line(call, code);
        }
        *inside_run = synthesized;
    }

    /// Emit an already-selected descriptor call without losing checked argument provenance.
    /// `leading_non_argument_operands` covers a receiver retained separately by common IR but
    /// prepended to a static JVM invocation; it is never inferred from descriptor shape here.
    pub(super) fn emit_call_descriptor_operands(
        &mut self,
        call_expression: u32,
        leading_non_argument_operands: usize,
        ops: &[u32],
        physical: &[Ty],
        code: &mut CodeBuilder,
    ) -> Result<(), DescriptorArityMismatch> {
        DescriptorArityMismatch::check(None, ops.len(), physical.len())?;
        let mut index = 0usize;
        // The call owns any operand a default-argument realization synthesized for it, so its own
        // line goes back into effect at the start of each such run.
        let origins = self.default_operand_origins(call_expression, ops, false);
        self.emit_operands_adapted(
            Some((call_expression, &origins)),
            ops,
            code,
            |this, source, code| {
                let physical_index = index;
                let expression = ops[physical_index];
                let target = physical[physical_index];
                index += 1;
                if let Some(argument_index) =
                    call_argument_index(physical_index, leading_non_argument_operands)
                {
                    this.adapt_physical_call_operand_for(
                        call_expression,
                        argument_index,
                        expression,
                        source,
                        target,
                        code,
                    );
                } else {
                    // A retained dispatch/extension receiver is a physical JVM operand, but not an
                    // entry in the call's checked argument vector. Materialize it at the selected
                    // descriptor slot without shifting argument-owned provenance onto it.
                    this.adapt_physical_operand_for(expression, source, target, code);
                }
            },
        );
        Ok(())
    }

    pub(super) fn emit_descriptor_virtual_operands(
        &mut self,
        call_expression: u32,
        target: VirtualCallTarget<'_>,
        receiver: u32,
        args: &[u32],
        physical_params: &[Ty],
        code: &mut CodeBuilder,
    ) -> Result<(), DescriptorArityMismatch> {
        let owner = target.owner;
        DescriptorArityMismatch::check(Some(target), args.len(), physical_params.len())?;
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
        self.emit_operands_adapted(None, &ops, code, |this, source, code| {
            let operand = ops[index];
            let target = physical[index];
            if index == 0 {
                // A dispatch receiver is materialized at the class the call names only out of an
                // erased `Object`: kotlinc names the receiver's own class, so it never widens one,
                // and an `invokespecial` must see the current class, not the interface it calls.
                let target = if source.is_reference()
                    && !super::jvm_is_erased_top(super::ir_ty_to_jvm(&source))
                {
                    source
                } else {
                    target
                };
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
    use super::{call_argument_index, DescriptorArityMismatch, VirtualCallTarget};
    use crate::ir::{Callee, IrConst, IrExpr, IrFile, IrFunction};
    use crate::jvm::ir_emit::invariant_tests::emit_for_test;
    use crate::jvm::ir_emit::EmitRun;
    use crate::types::Ty;

    #[test]
    fn a_retained_receiver_does_not_shift_argument_owned_continuation_provenance() {
        let mut provenance = crate::jvm::default_call_operands::DefaultCallOperands::default();
        assert!(provenance.record_continuation(7, 1));

        let receiver = call_argument_index(0, 1);
        let first_argument = call_argument_index(1, 1);
        let continuation = call_argument_index(2, 1);
        assert_eq!(receiver, None);
        assert_eq!(first_argument, Some(0));
        assert_eq!(continuation, Some(1));
        assert!(!first_argument.is_some_and(|index| provenance.is_continuation(7, index)));
        assert!(continuation.is_some_and(|index| provenance.is_continuation(7, index)));
    }

    #[test]
    fn arity_mismatch_keeps_the_exact_realized_callable() {
        let mismatch = DescriptorArityMismatch::check(
            Some(VirtualCallTarget {
                owner: "sample/Backend",
                name: "remove",
                descriptor: "(Ljava/lang/String;I)Z",
            }),
            1,
            2,
        )
        .unwrap_err();

        assert_eq!(
            mismatch.target.as_ref().map(ToString::to_string),
            Some("sample/Backend.remove(Ljava/lang/String;I)Z".to_string())
        );
    }

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
