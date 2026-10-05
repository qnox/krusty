//! The unified host+lambda byte splice: a possibly branchy classpath `inline fun` body relocated
//! into the caller, with each `FunctionN.invoke` of a selected literal lambda parameter replaced by
//! that literal's body. `require(cond) { msg }`, `check(cond) { msg }`, and the branchless
//! `let`/`also`/… shapes all take it.
//!
//! The call reaches this splice after its literal positions are selected and its route is decided.
//! [`unified_splice_plan`] then decides, before any capture is bound or any code is emitted:
//!
//! - each selected literal's checked facts (its function type sized to its arity, its body's
//!   result, a method parameter per argument). A missing or mis-sized fact is invalid IR, and the
//!   call fails with an emission error; nothing is reconstructed from the physical lambda method;
//! - the host body's frame plan around those literals. A host whose frame cannot be planned (one
//!   whose invokes move the lambda through another local, or a coroutine state machine compiled
//!   for direct calls) is a shape this splice does not support. That is decided here, before
//!   placement, so the call is [`InlineCallOutcome::NotApplicable`] and its caller lowers it as
//!   for any other declined splice: a real call when the callee allows one, an error otherwise.
//!
//! Once the plan is made the splice owns the call: failing to build, decode, or splice a placed
//! literal is an emission error, never another lowering.

use super::inline_call_outcome::InlineCallOutcome;
use super::*;
use crate::jvm::inline::SplicedFrame;

/// The checked facts the placement of one selected literal reads.
#[derive(Debug, PartialEq)]
pub(super) struct LiteralFacts {
    /// The literal lambda expression.
    lambda: ExprId,
    /// Its implementation method, whose parameters are `[captures…, arguments…]`.
    impl_fn: crate::ir::FunId,
    /// Its checked body.
    inline_body: ExprId,
    /// How many captures precede the arguments among the method's parameters.
    captures: usize,
    /// The method parameter each argument is stored to.
    physical: Vec<Ty>,
    /// The semantic type of each argument, which selects how it crosses `invoke`.
    semantic: Vec<Ty>,
    /// The checked type of the body's value.
    result: Ty,
}

/// One selected literal, placed in the host's frame.
#[derive(Debug, PartialEq)]
pub(super) struct PlacedLiteral {
    /// Its position among the call's operands.
    operand: usize,
    facts: LiteralFacts,
    /// The first caller slot free at every invoke site of this literal.
    slot_base: u16,
    /// How many invoke sites the host has for this literal.
    sites: usize,
}

/// The validated plan of one unified splice.
#[derive(Debug, PartialEq)]
pub(super) struct UnifiedSplicePlan {
    /// The selected literals, in operand order.
    literals: Vec<PlacedLiteral>,
    /// One past the highest caller slot the relocated host body occupies.
    top_local: u16,
}

/// The checked facts of the selected literal `lambda`.
fn literal_facts(ir: &IrFile, lambda: ExprId) -> Result<LiteralFacts, &'static str> {
    let IrExpr::Lambda {
        impl_fn,
        arity,
        captures,
        inline_body: Some(inline_body),
        ..
    } = ir.expr(lambda)
    else {
        return Err("a placed lambda is not a literal inline lambda");
    };
    let arity = *arity as usize;
    // `arity` is the source-level arity. It cannot recover the capture boundary after the suspend
    // pass appends a physical `Continuation` parameter; the capture list is that exact boundary.
    let physical = jvm_function_params(ir, *impl_fn);
    let Some(arguments) = physical.get(captures.len()..captures.len() + arity) else {
        return Err("a placed lambda's method lacks a parameter for each argument");
    };
    let semantic = match ir.logical_types.get(&lambda).map(|ty| ty.non_null()) {
        Some(Ty::Fun(signature)) if signature.params.len() == arity => signature.params.clone(),
        _ => return Err("a placed lambda has no checked function type with each argument's type"),
    };
    // The body is built in a scratch frame whose argument slots are not installed yet, so its
    // value's type is the checked one, never read back from slots or from the method.
    let Some(&result) = ir.logical_types.get(inline_body) else {
        return Err("a placed lambda's body has no checked result type");
    };
    Ok(LiteralFacts {
        lambda,
        impl_fn: *impl_fn,
        inline_body: *inline_body,
        captures: captures.len(),
        physical: arguments.to_vec(),
        semantic,
        result,
    })
}

/// What planning a unified splice decided.
#[derive(Debug, PartialEq)]
pub(super) enum UnifiedSplice {
    /// The splice is planned; placing it owns the call.
    Planned(UnifiedSplicePlan),
    /// The host body's frame cannot be planned around the selected literals: a host shape this
    /// splice does not support, decided before anything is placed.
    UnsupportedHost,
}

/// Plan the splice of the literals at `positions` among `args`, whose frame plan is `frame`.
/// Every selected literal must carry its checked facts, whatever the host; a planned frame must
/// place every selected literal at one or more invoke sites.
pub(super) fn unified_splice_plan(
    ir: &IrFile,
    args: &[ExprId],
    positions: &[usize],
    frame: Option<&SplicedFrame>,
) -> Result<UnifiedSplice, &'static str> {
    let facts = positions
        .iter()
        .map(|&operand| {
            let lambda = *args
                .get(operand)
                .ok_or("a placed lambda's position is not an operand of the call")?;
            Ok((operand, literal_facts(ir, lambda)?))
        })
        .collect::<Result<Vec<_>, &'static str>>()?;
    let Some(frame) = frame else {
        return Ok(UnifiedSplice::UnsupportedHost);
    };
    let mut literals = Vec::with_capacity(facts.len());
    for (ordinal, (operand, facts)) in facts.into_iter().enumerate() {
        let (Some(&slot_base), Some(&sites)) = (
            frame.lambda_bases.get(ordinal),
            frame.site_counts.get(ordinal),
        ) else {
            return Err("a placed lambda has no place in the inline body's frame plan");
        };
        if sites == 0 {
            return Err("a placed lambda has no invoke site in the inline body");
        }
        literals.push(PlacedLiteral {
            operand,
            facts,
            slot_base,
            sites,
        });
    }
    Ok(UnifiedSplice::Planned(UnifiedSplicePlan {
        literals,
        top_local: frame.top_local,
    }))
}

/// The spliced code's handlers clear the operand stack, and its external transfers target code
/// outside the splice; neither is sound with caller operands below the call.
const OPERANDS_ACROSS_SPLICED_CONTROL: &str =
    "a spliced inline body transfers control with caller operands on the stack";

impl Emitter<'_> {
    /// Splice `call`, whose selected literal positions are `positions`, with the host's locals from
    /// `base`. [`InlineCallOutcome::NotApplicable`] only when the host's shape is unsupported, which
    /// is decided before anything is placed; once placed, a failure is the run's emission error.
    pub(super) fn splice_selected_literals(
        &mut self,
        call: &bytecode_inline_call::ClasspathInlineCall<'_, '_>,
        positions: &[usize],
        base: u16,
        code: &mut CodeBuilder,
    ) -> InlineCallOutcome {
        let (params, plan) = match self.plan_unified_splice(call, positions, base, code) {
            Ok((params, UnifiedSplice::Planned(plan))) => (params, plan),
            Ok((_, UnifiedSplice::UnsupportedHost)) => {
                crate::trace_compiler!(
                    "splice",
                    "unified splice declined before placement: {}.{}{}",
                    call.target.owner,
                    call.target.name,
                    call.target.splice_desc
                );
                return InlineCallOutcome::NotApplicable;
            }
            Err(reason) => {
                self.run.set_emit_error(reason.to_string());
                return InlineCallOutcome::HandledWithError;
            }
        };
        match self.splice_unified_call(call, positions, base, (&params, &plan), code) {
            Ok(()) => InlineCallOutcome::Handled,
            Err(reason) => {
                self.run.set_emit_error(reason.to_string());
                InlineCallOutcome::HandledWithError
            }
        }
    }

    /// Decide the splice of `call` from the checked IR and the host body, touching nothing. Also
    /// returns the callee's physical parameters, one per operand.
    fn plan_unified_splice(
        &self,
        call: &bytecode_inline_call::ClasspathInlineCall<'_, '_>,
        positions: &[usize],
        base: u16,
        code: &CodeBuilder,
    ) -> Result<(Vec<Ty>, UnifiedSplice), &'static str> {
        let bytecode_inline_call::ClasspathInlineCall {
            target, args, body, ..
        } = *call;
        let descriptor = target.splice_desc;
        let params = parse_descriptor_params(descriptor)
            .filter(|params| params.len() == args.len())
            .ok_or("an inline callee's descriptor does not match the call's operands")?;
        // ONE plan for caller locals: where the relocated host body ends, and where each
        // substituted lambda's own locals begin. A substituted lambda's parameter slot is closed by
        // the splice, so the body occupies that many slots fewer; and a lambda's locals go at the
        // first slot free WHERE ITS INVOKE IS, not above every host local, because the reference
        // compiler reuses slots belonging to host locals that are not written yet.
        let frame = crate::jvm::inline::spliced_frame(body, descriptor, positions, base);
        let plan = unified_splice_plan(self.ir, args, positions, frame.as_ref())?;
        // The host's own handlers clear the operand stack: with caller operands below the call,
        // the host is a shape this splice does not support.
        if !body.handlers.is_empty() && code.stack_height() != 0 {
            return Ok((params, UnifiedSplice::UnsupportedHost));
        }
        Ok((params, plan))
    }

    fn splice_unified_call(
        &mut self,
        call: &bytecode_inline_call::ClasspathInlineCall<'_, '_>,
        positions: &[usize],
        base: u16,
        (params, plan): (&[Ty], &UnifiedSplicePlan),
        code: &mut CodeBuilder,
    ) -> Result<(), &'static str> {
        let bytecode_inline_call::ClasspathInlineCall {
            call_expression,
            target,
            args,
            body,
            ..
        } = *call;
        let callee = target.name;
        let inline_only = target.inline_only;
        let descriptor = target.splice_desc;
        crate::trace_compiler!(
            "splice",
            "inline operands {:?}",
            args.iter()
                .map(|&argument| (argument, self.ir.expr(argument), self.value_ty(argument)))
                .collect::<Vec<_>>()
        );
        self.frame.reserve_through(plan.top_local);
        // Capture initializers belong to lambda-creation time, in argument evaluation order. A
        // capture that is already a caller local needs no code; every other checked value is
        // materialized once when its lambda operand is reached, then the spliced body reads that
        // stable slot. This is required for nested inline lambdas (the captured value can itself be
        // a lambda), and also preserves side effects if a future capture initializer is not pure.
        let mut capture_materializations: Vec<(usize, u32, u16, Ty)> = Vec::new();
        let mut lam_splices: Vec<crate::jvm::inline::LambdaSplice> = Vec::new();
        // The deepest operand stack any spliced lambda body reaches: the host's `max_stack` must
        // cover it, since the body is inlined into the host (a deep lambda body, e.g.
        // `123 != intArrayOf() as Any`, would otherwise overflow the host's stack).
        let mut lam_max_stack = 0u16;
        for literal in &plan.literals {
            // Each capture binds to the caller's actual slot (a mutable capture writes through); a
            // materialized one is left with the rest of the call's frame when it finishes.
            let cap_bindings = self.bind_placed_captures(
                literal.facts.lambda,
                literal.operand,
                &mut capture_materializations,
            )?;
            let (bodies, lam_max_locals, lam_stack) =
                self.build_placed_bodies(callee, literal, &cap_bindings)?;
            if code.max_locals < lam_max_locals {
                code.max_locals = lam_max_locals;
            }
            self.frame.reserve_through(lam_max_locals);
            lam_max_stack = lam_max_stack.max(lam_stack);
            lam_splices.push(crate::jvm::inline::LambdaSplice {
                param_index: literal.operand,
                bodies,
            });
        }
        // Probe at offset 0. Switch padding and absolute handler/external-transfer offsets require a
        // second splice at the method's real byte offset; relative branches do not.
        let probe =
            crate::jvm::inline::splice_unified(body, descriptor, base, &lam_splices, 0, self.cw)
                .ok_or("an inline body cannot be spliced around its placed lambdas")?;
        let needs_relayout = probe.needs_relayout;
        // Ordinary branches preserve the caller's operand prefix and final-body dataflow computes
        // it. A handler clears that prefix, while an external transfer targets code outside the
        // splice; neither is sound until the surrounding operands have been spilled.
        let needs_empty_stack = !probe.handlers.is_empty() || !probe.external_branches.is_empty();
        if needs_empty_stack && code.stack_height() != 0 {
            return Err(OPERANDS_ACROSS_SPLICED_CONTROL);
        }
        let ret_words = descriptor_ret_words(descriptor);
        // Emit each NON-lambda argument (the operands the host prologue stores into its parameter
        // slots), and each placed literal's materialized captures in its place.
        let mut arg_words = 0i32;
        for (i, &a) in args.iter().enumerate() {
            if positions.contains(&i) {
                for &(_, capture, slot, ty) in capture_materializations
                    .iter()
                    .filter(|(argument, ..)| *argument == i)
                {
                    self.emit_value(capture, code);
                    self.adapt_physical_operand_for(capture, self.value_ty(capture), ty, code);
                    store(ty, slot, code);
                }
                continue;
            }
            self.emit_value(a, code);
            let at = self.value_ty(a);
            self.adapt_physical_call_operand_for(call_expression, i, a, at, params[i], code);
            arg_words += slot_words(params[i]) as i32;
        }
        if !needs_relayout {
            // Position-independent host + lambda: append the probed bytes at any stack height. The
            // host's stack must cover the host body PLUS the deepest spliced lambda body (a safe
            // upper bound on the real peak).
            let ret_words = if probe.falls_through { ret_words } else { 0 };
            let splice_start = code.bytes.len();
            code.splice_inline(
                &probe.bytes,
                &probe.external_branches,
                body.max_stack + lam_max_stack,
                plan.top_local,
                arg_words,
                ret_words,
                probe.falls_through,
            );
            self.record_spliced_lines(&probe.lines, body, inline_only, splice_start, code);
            self.record_spliced_locals(&probe.locals, inline_only, splice_start, code);
            return Ok(());
        }
        // RE-splice at the real method offset so any switch in the host/lambda body pads correctly.
        let splice_start = code.bytes.len();
        let bs = crate::jvm::inline::splice_unified(
            body,
            descriptor,
            base,
            &lam_splices,
            splice_start,
            self.cw,
        )
        .ok_or("an inline body cannot be spliced around its placed lambdas")?;
        // Register the spliced body's relocated exception handlers (try/catch/finally from `use`/
        // `synchronized`/`runCatching`). Final-body analysis derives their handler-entry frames.
        bind_inline_handlers(code, &bs.handlers);
        let ret_words = if bs.falls_through { ret_words } else { 0 };
        code.splice_inline(
            &bs.bytes,
            &bs.external_branches,
            body.max_stack + lam_max_stack,
            plan.top_local,
            arg_words,
            ret_words,
            bs.falls_through,
        );
        self.record_spliced_lines(&bs.lines, body, inline_only, 0, code);
        self.record_spliced_locals(&bs.locals, inline_only, 0, code);
        Ok(())
    }

    /// Build the body of the placed `literal`, once per invoke site when it suspends, each leaving
    /// its boxed result on the stack. Returns the bodies, and the locals and stack they reach.
    fn build_placed_bodies(
        &mut self,
        callee: &str,
        literal: &PlacedLiteral,
        cap_bindings: &inline_lambda_aliases::CaptureBindings,
    ) -> Result<(Vec<crate::jvm::inline::LambdaBody>, u16, u16), &'static str> {
        let facts = &literal.facts;
        let arity = facts.physical.len();
        let n_cap = facts.captures;
        crate::trace_compiler!(
            "splice",
            "inline lambda expression={} impl={} physical={:?} semantic={:?}",
            facts.lambda,
            facts.impl_fn,
            facts.physical,
            facts.semantic
        );
        // Above the host's frame at the invoke, and above every slot this lambda's own captures
        // occupy: a capture is live for the whole body that reads it, so a parameter placed on one
        // overwrites the value the body was given.
        let lambda_slot_base = literal.slot_base.max(cap_bindings.ceiling());
        // One body serves every invoke site unless the body carries a suspension: then each site is
        // a state of this machine, and a state is one position with its own spill set, so the body
        // is built once per site and each copy marks its suspensions with its own ordinals. The
        // copies are laid out from the same slot base, so they differ in nothing but those
        // ordinals — which is what lets the discovery pass and the build pass number them alike.
        let copies = self.frame.mark();
        let states_before = self.machine_next_ordinal;
        let mut bodies: Vec<crate::jvm::inline::LambdaBody> = Vec::new();
        let mut lam_max_locals = 0u16;
        let mut lam_stack = 0u16;
        loop {
            self.frame.rewind_to(copies);
            // The host left the lambda's `arity` arguments on the stack (as `Object`, the erased
            // `FunctionN.invoke` parameters); each is coerced to the parameter the body takes and
            // stored (top = last). Then the body runs, and its result is boxed to `Object`, the
            // replaced `invoke`'s result.
            let mut scratch = CodeBuilder::new(self.frame.size());
            scratch.set_stack(arity as u16);
            let mut lam_locals_declared: Vec<(u16, u16, String, String)> = Vec::new();
            let mut lambda_slot = lambda_slot_base;
            let mut param_slots: Vec<(u16, Ty)> = cap_bindings.slots.clone();
            param_slots.extend(std::iter::repeat_n((0u16, Ty::Error), arity));
            for j in (0..arity).rev() {
                // A value class the implementation takes boxed, the inline body takes unboxed.
                let jt =
                    self.coerce_invoke_argument(facts.semantic[j], facts.physical[j], &mut scratch);
                let slot = lambda_slot;
                lambda_slot += slot_words(jt);
                self.frame.reserve_through(lambda_slot);
                store(jt, slot, &mut scratch);
                param_slots[n_cap + j] = (slot, jt);
                // Its scope opens once the store completes, and runs to the end of the body.
                if self.record_locals {
                    if let Some(name) = self
                        .ir
                        .fn_params
                        .get(&facts.impl_fn)
                        .and_then(|info| info.identities.get(n_cap + j))
                        .and_then(|identity| identity.source_name.as_ref())
                    {
                        lam_locals_declared.push((
                            u16::try_from(scratch.bytes.len()).unwrap_or(u16::MAX),
                            slot,
                            name.clone(),
                            crate::jvm::names::type_descriptor(jt),
                        ));
                    }
                }
            }
            // The reference compiler opens an inlined lambda body with its own inline-depth marker
            // — `iconst_0; istore` into a `$i$a$-<callee>-<caller>` local — exactly as it opens an
            // inlined function body with `$i$f$<callee>`. The host's marker arrives inside the
            // relocated host body; this one has no other source, because the lambda body is emitted
            // from IR rather than relocated.
            let depth_marker = lambda_slot;
            lambda_slot += 1;
            self.frame.reserve_through(lambda_slot);
            scratch.push_int(0, self.cw);
            store(Ty::Int, depth_marker, &mut scratch);
            if self.record_locals {
                let marker = crate::jvm::debug_local_names::spliced_lambda_marker_name(
                    self.ir,
                    callee,
                    facts.impl_fn,
                )
                .ok_or("a spliced lambda frame has no realized class provenance")?;
                lam_locals_declared.push((
                    u16::try_from(scratch.bytes.len()).unwrap_or(u16::MAX),
                    depth_marker,
                    marker,
                    "I".to_string(),
                ));
            }
            let body_ret = self.emit_fn_body_inline_with_aliases(
                facts.inline_body,
                &param_slots,
                cap_bindings.aliases.clone(),
                &mut scratch,
            );
            // The erased `invoke` result is `Object`. Coerce from the BODY's value type, not the
            // contextual lambda declaration return: a block accepted as `() -> Any?` can still
            // produce a primitive `Boolean`/`Int` here.
            self.coerce_invoke_result(facts.result, body_ret, &mut scratch);
            scratch.link_local_branches(); // enclosing-loop transfers remain owned by the caller
            let lam_insns = crate::jvm::inline::disassemble_lambda(
                &scratch.bytes,
                &scratch.external_branches(),
            )
            .ok_or("a placed lambda's built body cannot be decoded for the splice")?;
            lam_max_locals = lam_max_locals.max(scratch.max_locals);
            lam_stack = lam_stack.max(scratch.max_stack);
            bodies.push(crate::jvm::inline::LambdaBody {
                body: lam_insns,
                locals: lam_locals_declared,
                lines: scratch.line_marks().to_vec(),
                handlers: scratch.resolved_exceptions(),
            });
            let suspends = self.machine_next_ordinal > states_before;
            if !suspends || bodies.len() >= literal.sites {
                break;
            }
        }
        Ok((bodies, lam_max_locals, lam_stack))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::IrFunction;
    use crate::jvm::classreader::MethodCode;

    /// A literal lambda capturing caller value 0 and taking one argument, whose method takes
    /// `method_params`.
    fn literal(ir: &mut IrFile, method_params: Vec<Ty>) -> (ExprId, ExprId) {
        let impl_fn = ir.add_fun(IrFunction {
            name: "placed$lambda".to_string(),
            params: method_params,
            ret: Ty::Int,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let capture = ir.add_expr(IrExpr::GetValue(0));
        let body = ir.add_expr(IrExpr::GetValue(1));
        let lambda = ir.add_expr(IrExpr::Lambda {
            impl_fn,
            arity: 1,
            captures: vec![capture],
            sam: None,
            inline_body: Some(body),
        });
        (lambda, body)
    }

    fn function_type(params: Vec<Ty>, ret: Ty) -> Ty {
        Ty::fun(params, ret)
    }

    fn frame(bases: Vec<u16>, sites: Vec<usize>) -> SplicedFrame {
        SplicedFrame {
            lambda_bases: bases,
            site_counts: sites,
            top_local: 7,
        }
    }

    /// The call `host(receiver, literal)`: one ordinary operand, then the literal.
    fn call_with_literal(ir: &mut IrFile) -> (Vec<ExprId>, ExprId, ExprId) {
        let receiver = ir.add_expr(IrExpr::GetValue(2));
        let (lambda, body) = literal(ir, vec![Ty::Long, Ty::Int]);
        (vec![receiver, lambda], lambda, body)
    }

    #[test]
    fn a_plan_reads_every_fact_from_checked_ir_and_the_frame_plan() {
        let mut ir = IrFile::default();
        let (args, lambda, body) = call_with_literal(&mut ir);
        ir.logical_types
            .insert(lambda, function_type(vec![Ty::UInt], Ty::UInt));
        ir.logical_types.insert(body, Ty::UInt);
        let impl_fn = match ir.expr(lambda) {
            IrExpr::Lambda { impl_fn, .. } => *impl_fn,
            _ => unreachable!(),
        };
        assert_eq!(
            unified_splice_plan(&ir, &args, &[1], Some(&frame(vec![4], vec![2]))),
            Ok(UnifiedSplice::Planned(UnifiedSplicePlan {
                literals: vec![PlacedLiteral {
                    operand: 1,
                    facts: LiteralFacts {
                        lambda,
                        impl_fn,
                        inline_body: body,
                        captures: 1,
                        physical: vec![Ty::Int],
                        semantic: vec![Ty::UInt],
                        result: Ty::UInt,
                    },
                    slot_base: 4,
                    sites: 2,
                }],
                top_local: 7,
            }))
        );
    }

    #[test]
    fn a_missing_semantic_function_type_rejects_the_splice() {
        let mut ir = IrFile::default();
        let (args, _, body) = call_with_literal(&mut ir);
        ir.logical_types.insert(body, Ty::Int);
        assert_eq!(
            unified_splice_plan(&ir, &args, &[1], Some(&frame(vec![4], vec![1]))),
            Err("a placed lambda has no checked function type with each argument's type")
        );
    }

    #[test]
    fn a_mis_sized_semantic_function_type_rejects_the_splice() {
        let mut ir = IrFile::default();
        let (args, lambda, body) = call_with_literal(&mut ir);
        ir.logical_types.insert(body, Ty::Int);
        for params in [Vec::new(), vec![Ty::Int, Ty::Int]] {
            ir.logical_types
                .insert(lambda, function_type(params, Ty::Int));
            assert_eq!(
                unified_splice_plan(&ir, &args, &[1], Some(&frame(vec![4], vec![1]))),
                Err("a placed lambda has no checked function type with each argument's type")
            );
        }
    }

    #[test]
    fn a_non_function_semantic_type_rejects_the_splice() {
        let mut ir = IrFile::default();
        let (args, lambda, body) = call_with_literal(&mut ir);
        ir.logical_types.insert(body, Ty::Int);
        ir.logical_types.insert(lambda, Ty::Int);
        assert_eq!(
            unified_splice_plan(&ir, &args, &[1], Some(&frame(vec![4], vec![1]))),
            Err("a placed lambda has no checked function type with each argument's type")
        );
    }

    #[test]
    fn a_missing_checked_body_result_rejects_the_splice() {
        let mut ir = IrFile::default();
        let (args, lambda, _) = call_with_literal(&mut ir);
        ir.logical_types
            .insert(lambda, function_type(vec![Ty::Int], Ty::Int));
        assert_eq!(
            unified_splice_plan(&ir, &args, &[1], Some(&frame(vec![4], vec![1]))),
            Err("a placed lambda's body has no checked result type")
        );
    }

    #[test]
    fn a_method_without_a_parameter_per_argument_rejects_the_splice() {
        let mut ir = IrFile::default();
        let receiver = ir.add_expr(IrExpr::GetValue(2));
        let (lambda, body) = literal(&mut ir, vec![Ty::Long]);
        ir.logical_types
            .insert(lambda, function_type(vec![Ty::Int], Ty::Int));
        ir.logical_types.insert(body, Ty::Int);
        assert_eq!(
            unified_splice_plan(
                &ir,
                &[receiver, lambda],
                &[1],
                Some(&frame(vec![4], vec![1]))
            ),
            Err("a placed lambda's method lacks a parameter for each argument")
        );
    }

    #[test]
    fn a_selected_operand_that_is_not_a_literal_rejects_the_splice() {
        let mut ir = IrFile::default();
        let (args, _, _) = call_with_literal(&mut ir);
        assert_eq!(
            unified_splice_plan(&ir, &args, &[0], Some(&frame(vec![4], vec![1]))),
            Err("a placed lambda is not a literal inline lambda")
        );
    }

    /// Fully checked facts, for the frame-plan cases.
    fn checked_call(ir: &mut IrFile) -> Vec<ExprId> {
        let (args, lambda, body) = call_with_literal(ir);
        ir.logical_types
            .insert(lambda, function_type(vec![Ty::Int], Ty::Int));
        ir.logical_types.insert(body, Ty::Int);
        args
    }

    #[test]
    fn a_host_without_a_frame_plan_is_unsupported_before_placement() {
        let mut ir = IrFile::default();
        let args = checked_call(&mut ir);
        assert_eq!(
            unified_splice_plan(&ir, &args, &[1], None),
            Ok(UnifiedSplice::UnsupportedHost)
        );
    }

    /// The checked facts are decided before the host's shape: an unsupported host never turns
    /// invalid IR into a declined call.
    #[test]
    fn missing_checked_facts_fail_even_where_the_host_is_unsupported() {
        let mut ir = IrFile::default();
        let (args, lambda, _) = call_with_literal(&mut ir);
        ir.logical_types
            .insert(lambda, function_type(vec![Ty::Int], Ty::Int));
        assert_eq!(
            unified_splice_plan(&ir, &args, &[1], None),
            Err("a placed lambda's body has no checked result type")
        );
        let (args, _, body) = call_with_literal(&mut ir);
        ir.logical_types.insert(body, Ty::Int);
        assert_eq!(
            unified_splice_plan(&ir, &args, &[1], None),
            Err("a placed lambda has no checked function type with each argument's type")
        );
    }

    #[test]
    fn a_frame_plan_without_the_literal_rejects_the_splice() {
        let mut ir = IrFile::default();
        let args = checked_call(&mut ir);
        assert_eq!(
            unified_splice_plan(&ir, &args, &[1], Some(&frame(Vec::new(), Vec::new()))),
            Err("a placed lambda has no place in the inline body's frame plan")
        );
        assert_eq!(
            unified_splice_plan(&ir, &args, &[1], Some(&frame(vec![4], vec![0]))),
            Err("a placed lambda has no invoke site in the inline body")
        );
    }

    /// A classpath `host(block: () -> Unit)` with `code`, over a pool naming
    /// `Function0.invoke()Ljava/lang/Object;` at 6 and `kotlin/_Assertions.$assertionsDisabled Z`
    /// at 12. Reading that field is a callee shape the MethodInliner port leaves to the splice.
    struct Host(Vec<u8>);

    impl crate::jvm::inline::MethodBodies for Host {
        fn body(&self, _owner: &str, _name: &str, _descriptor: &str) -> Option<MethodCode> {
            use crate::jvm::classreader::C;
            Some(MethodCode {
                max_stack: 1,
                max_locals: 1,
                code: self.0.clone(),
                source_cp: vec![
                    C::Other,
                    C::Utf8("kotlin/jvm/functions/Function0".to_string()),
                    C::Class(1),
                    C::Utf8("invoke".to_string()),
                    C::Utf8("()Ljava/lang/Object;".to_string()),
                    C::NameAndType(3, 4),
                    C::InterfaceMethodref(2, 5),
                    C::Utf8("kotlin/_Assertions".to_string()),
                    C::Class(7),
                    C::Utf8("$assertionsDisabled".to_string()),
                    C::Utf8("Z".to_string()),
                    C::NameAndType(9, 10),
                    C::Fieldref(8, 11),
                ]
                .into(),
                stackmap: None,
                handlers: Vec::new(),
                locals: Vec::new(),
                lines: Vec::new(),
                source_file: None,
                defining_class: "lib/HostKt".to_string(),
                dependency_source_map: None,
                bootstrap_methods: Vec::new(),
            })
        }
    }

    /// `invokeinterface Function0.invoke; pop; getstatic $assertionsDisabled; pop; return`.
    const INVOKE_AND_READ_ASSERTIONS: [u8; 11] = [0xb9, 0, 6, 1, 0, 0x57, 0xb2, 0, 12, 0x57, 0xb1];

    /// Emit `box() { host { } }`, where `host` is a public inline function that a real call could
    /// legally reach. Returns whether the file was emitted, the run's emission error, and its
    /// inline-failure category.
    fn emit_box_calling(host: Vec<u8>) -> (bool, Option<String>, Option<String>) {
        let mut ir = IrFile::default();
        let block = ir.add_expr(IrExpr::Block {
            stmts: Vec::new(),
            value: None,
        });
        let impl_fn = ir.add_fun(IrFunction {
            name: "box$lambda$0".to_string(),
            params: Vec::new(),
            ret: Ty::Unit,
            body: Some(block),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let inline_body = ir.add_expr(IrExpr::Block {
            stmts: Vec::new(),
            value: None,
        });
        let lambda = ir.add_expr(IrExpr::Lambda {
            impl_fn,
            arity: 0,
            captures: Vec::new(),
            sam: None,
            inline_body: Some(inline_body),
        });
        let block_type = function_type(Vec::new(), Ty::Unit);
        ir.logical_types.insert(lambda, block_type);
        ir.logical_types.insert(inline_body, Ty::Unit);
        let call = ir.add_expr(IrExpr::Call {
            callee: crate::ir::Callee::Static {
                owner: crate::types::type_name("lib/HostKt"),
                name: "host".to_string(),
                descriptor: "(Lkotlin/jvm/functions/Function0;)V".to_string(),
                inline: crate::libraries::InlineKind::CanInline,
            },
            dispatch_receiver: None,
            args: vec![lambda],
        });
        ir.call_inline_modifiers.insert(
            call,
            vec![crate::types::InlineParameterModifier::None].into(),
        );
        ir.call_declared_params
            .insert(call, vec![block_type].into());
        ir.add_fun(IrFunction {
            name: "box".to_string(),
            params: Vec::new(),
            ret: Ty::Unit,
            body: Some(call),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let run = EmitRun::default();
        let emitted = crate::jvm::ir_emit::invariant_tests::emit_for_test_with_bodies(
            &ir,
            "CallerKt",
            &run,
            &Host(host),
        )
        .is_some();
        (emitted, run.emit_error(), run.inline_bail())
    }

    /// The host's invoke reads no parameter, so no frame plan places the literal: a host shape the
    /// splice does not support, decided before anything is placed. The call is declined and, as
    /// `host` allows, called for real.
    #[test]
    fn a_host_whose_frame_cannot_be_planned_declines_before_placement() {
        let mut host = vec![0x01];
        host.extend(INVOKE_AND_READ_ASSERTIONS);
        assert_eq!(emit_box_calling(host), (true, None, None));
    }

    /// The literal is selected, placed, and its body built; the host's read of `_Assertions` then
    /// cannot be spliced. The call fails after placement; it is never called for real.
    #[test]
    fn a_placed_literal_whose_splice_cannot_be_realized_fails_the_call() {
        let mut host = vec![0x2a];
        host.extend(INVOKE_AND_READ_ASSERTIONS);
        assert_eq!(
            emit_box_calling(host),
            (
                false,
                Some("an inline body cannot be spliced around its placed lambdas".into()),
                None
            )
        );
    }
}
