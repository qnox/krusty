//! A call to a classpath inline function, inlined by the port of kotlinc's `MethodInliner`
//! ([`crate::jvm::inliner`]): the call-site half of kotlinc's `IrInlineCodegen`.
//!
//! Each argument is evaluated and stored to its temporary as soon as it is evaluated
//! (`genValueAndPut`), unless it is read from the caller local it already lives in, or — for an
//! `@InlineOnly` callee whose body loads its parameters first — evaluated in place where the body
//! loads it (`InplaceArgumentsMethodTransformer`). The inlined body then follows.
//!
//! A call without literal lambda arguments that the port cannot transform remains a legal direct
//! call. A call whose body invokes a literal lambda takes the route [`LambdaCallRoute`] planned for
//! it before emission; the byte splice is never a fallback for either.

use std::collections::HashMap;

use super::*;

mod lambda_node;
mod lambda_route;
use lambda_node::callable_reference_template;
mod regenerated_objects;
use crate::jvm::bytecode_passes::coroutines::markers::{
    is_after_inline_marker, is_before_inline_marker,
};
use crate::jvm::inliner::{self, Binding, InlineError, Parameter, Parameters};
use crate::jvm::method_node::{
    encode_instruction, is_terminal, stack_shapes, word_delta, Category, Insn, MethodNode, Node,
};
use crate::jvm::source_map::SourceMap;
pub(super) use lambda_route::{LambdaCallRoute, SpliceReason};
pub(super) use regenerated_objects::{RegeneratedObjectNames, RegenerationSite};

const ACC_STATIC: u16 = 0x0008;

/// kotlinc's `IrDeclaration.isInlineOnly`: the `@InlineOnly` annotation, which the class file
/// records by making the method private. A public reified function is still mandatory to splice
/// and keeps the lines and locals an `@InlineOnly` body drops.
///
/// A multifile facade (`MapsKt`, `StringsKt`) does not declare the method. The call names the
/// facade; the private method, and the body, live on the part the facade extends. Privacy is the
/// defining class's bit, the same class [`MethodBodies::body`] reads.
pub(super) fn declaration_is_inline_only(
    bodies: &dyn crate::jvm::inline::MethodBodies,
    owner: &str,
    name: &str,
    descriptor: &str,
    inline: crate::libraries::InlineKind,
) -> bool {
    if !inline.must_inline() {
        return false;
    }
    if bodies.member_is_private(owner, name, descriptor) {
        return true;
    }
    bodies.body(owner, name, descriptor).is_some_and(|code| {
        code.defining_class != owner
            && bodies.member_is_private(&code.defining_class, name, descriptor)
    })
}

/// A static call the byte splice may absorb. `@InlineOnly` is decided here, from the defining
/// class, so the emitter facade does not assemble that target itself.
pub(super) struct StaticSpliceRequest<'a> {
    pub(super) call_expression: u32,
    pub(super) owner: &'a str,
    pub(super) name: &'a str,
    pub(super) descriptor: &'a str,
    pub(super) args: &'a [u32],
    pub(super) dispatch_receiver: Option<u32>,
    pub(super) inline: crate::libraries::InlineKind,
    pub(super) reified: &'a crate::jvm::reified_arguments::ReifiedArguments,
}

/// The clean failure of a reified inline body whose call shape only the byte splice handles: only
/// the MethodNode inliner specializes reified type parameters.
pub(super) const REIFIED_BODY_ON_BYTE_SPLICE: &str =
    "a reified inline body in a call shape only the byte splice handles";

/// The failure of a call whose selected literals its body uses only as values, when the bridge
/// cannot splice the body around them.
const VALUE_USED_LITERAL_UNSPLICED: &str =
    "an inline body that takes a literal lambda as a value cannot be spliced around it";

/// How the call site supplies one parameter.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Supply {
    /// Evaluated and stored to its temporary before the body.
    Stored,
    /// Evaluated where the body first loads its temporary.
    InPlace,
    /// Read from the caller local the argument lives in.
    CallerLocal,
}

/// The clean failure of an inline callee whose code the MethodNode reader rejects.
pub(super) const UNREADABLE_INLINE_BODY: &str =
    "an inline callee's code cannot be read as a method node";

/// Refuse the byte splice for a reified inline body: only the MethodNode inliner specializes its
/// reified type parameters. A body that cannot be read is refused too, never spliced unchecked.
pub(super) fn check_byte_splice_body(
    name: &str,
    descriptor: &str,
    body: &crate::jvm::classreader::MethodCode,
) -> Result<(), &'static str> {
    let callee =
        MethodNode::read(ACC_STATIC, name, descriptor, body).map_err(|_| UNREADABLE_INLINE_BODY)?;
    if inliner::has_reified_markers(&callee) {
        return Err(REIFIED_BODY_ON_BYTE_SPLICE);
    }
    Ok(())
}

impl Emitter<'_> {
    /// Splice a static inline call. Privacy for `@InlineOnly` is the defining class's bit, including
    /// a multifile part the facade extends.
    pub(super) fn try_splice_static_inline(
        &mut self,
        request: StaticSpliceRequest<'_>,
        code: &mut CodeBuilder,
    ) -> InlineCallOutcome {
        let StaticSpliceRequest {
            call_expression,
            owner,
            name,
            descriptor,
            args,
            dispatch_receiver,
            inline,
            reified,
        } = request;
        let for_inline_copy =
            self.ir.suspend_calls.contains_key(&call_expression) && !inline.must_inline();
        if let Some(recv) = dispatch_receiver {
            let recv_desc = type_descriptor(self.value_ty(recv));
            let splice_desc = format!("({}{}", recv_desc, &descriptor[1..]);
            let mut all = Vec::with_capacity(args.len() + 1);
            all.push(recv);
            all.extend(args.iter().copied());
            let target = super::inline_call::InlineStaticTarget {
                owner,
                name,
                descriptor,
                splice_desc: &splice_desc,
                inline_only: declaration_is_inline_only(
                    self.bodies,
                    owner,
                    name,
                    descriptor,
                    inline,
                ),
                allow_owner_bridge: true,
                for_inline_copy,
            };
            self.try_inline_static_as(call_expression, target, &all, 1, code, reified)
        } else {
            let has_lambda_arg = args.iter().any(|&argument| {
                matches!(self.ir.expr(argument), IrExpr::Lambda { .. })
                    || callable_reference_template(self.ir, argument).is_some()
                    || self.function_ref_class_and_captures(argument).is_some()
                    || self.property_ref_class_and_captures(argument).is_some()
            });
            let target = super::inline_call::InlineStaticTarget {
                owner,
                name,
                descriptor,
                splice_desc: descriptor,
                inline_only: declaration_is_inline_only(
                    self.bodies,
                    owner,
                    name,
                    descriptor,
                    inline,
                ),
                allow_owner_bridge: inline.must_inline() || has_lambda_arg,
                for_inline_copy,
            };
            self.try_inline_static_as(call_expression, target, args, 0, code, reified)
        }
    }

    /// Splice `owner.name` whose REAL (body-fetch) descriptor is `descriptor`, mapping the body's
    /// locals per `splice_desc`. For an ordinary static they are equal; for an INSTANCE inline
    /// method spliced through this path, `splice_desc` PREPENDS the receiver as the first parameter
    /// (`this` = local 0) and `args[0]` is that receiver — so the body's `aload_0`/`aload_1`/… map
    /// to receiver/params.
    ///
    /// [`InlineCallOutcome::NotApplicable`] only before a placement plan is committed: no body, a
    /// body this class cannot reach, a call without inlined literals the port does not cover, or an
    /// unsupported host whose selected literals can all legally be materialized for a real call.
    /// A non-local jump and an `@InlineOnly` target can never take that last decline.
    fn try_inline_static_as(
        &mut self,
        call_expression: u32,
        target: InlineStaticTarget<'_>,
        args: &[u32],
        leading_non_argument_operands: usize,
        code: &mut CodeBuilder,
        reified: &crate::jvm::reified_arguments::ReifiedArguments,
    ) -> InlineCallOutcome {
        let InlineStaticTarget {
            owner,
            name,
            descriptor,
            splice_desc,
            allow_owner_bridge,
            for_inline_copy,
            ..
        } = target;
        crate::trace_compiler!(
            "splice",
            "inline target {owner}.{name}{descriptor} splice_descriptor={splice_desc} args={}",
            args.len()
        );
        // Only a callable suspend inline function's `$$forInline` copy is spliced: its `name` body
        // is already the callee's own state machine. Without the copy the call declines (a
        // must-inline one bails), never splicing that machine.
        let for_inline;
        let body_name = if for_inline_copy {
            for_inline = format!("{name}$$forInline");
            for_inline.as_str()
        } else {
            name
        };
        let Some(body) = self.bodies.body(owner, body_name, descriptor) else {
            crate::trace_compiler!("splice", "no body for {owner}.{body_name}{descriptor}");
            return InlineCallOutcome::NotApplicable;
        };
        // A body that references a PRIVATE member (its own facade's helper or backing field) runs
        // legally only inside the defining class — spliced into the caller, the reference is an
        // IllegalAccessError (kotlinc rewrites it to a synthetic `access$…` bridge, which krusty
        // does not model). Decline: the caller emits a real call, which stays in the class.
        if crate::jvm::inline::references_private_member(
            &body.code,
            &body.source_cp,
            &body.bootstrap_methods,
            &mut |o, n, d| self.bodies.member_is_private(o, n, d),
            &mut |o, n, d| self.bodies.member_is_publicly_reachable(o, n, d),
            &mut |class| self.bodies.class_is_publicly_reachable(class),
        ) {
            crate::trace_compiler!(
                "splice",
                "inaccessible relocation dependency in {owner}.{name}{descriptor}"
            );
            return InlineCallOutcome::NotApplicable;
        }
        if !allow_owner_bridge && owner != methodref_owner(&body, name, descriptor).unwrap_or(owner)
        {
            crate::trace_compiler!(
                "splice",
                "owner-bridge mismatch for {owner}.{name}{descriptor} (real owner {:?})",
                methodref_owner(&body, name, descriptor)
            );
            return InlineCallOutcome::NotApplicable;
        }
        // Splice the body's locals above BOTH the slot allocator's next free slot and the code's
        // high-water mark, so the spliced temporaries can never collide with a caller local (live
        // or reserved-but-unstored).
        let base = self.frame.size().max(code.max_locals);
        let inline_call = ClasspathInlineCall {
            call_expression,
            target: &target,
            args,
            leading_non_argument_operands,
            body: &body,
            reified,
        };
        // Descriptor compatibility is an eligibility check, before any literal is selected and
        // this lowering owns the call. Suspend lowering can append a continuation to the physical
        // call while the classpath body still has its declaration descriptor; a public inline
        // method in that shape remains a legal real call. Once a matching call selects literals,
        // every later missing fact or realization failure is an emission error.
        if parse_descriptor_params(splice_desc)
            .is_none_or(|parameters| parameters.len() != args.len())
        {
            return InlineCallOutcome::NotApplicable;
        }
        let has_lambda_arg = args
            .iter()
            .any(|&argument| self.is_inline_lambda_shape(argument));
        if !has_lambda_arg {
            return InlineCallOutcome::declined_or_handled(
                self.try_inline_classpath_body(&inline_call, code),
            );
        }
        // kotlinc expands every literal whose parameter is not `noinline`; a `noinline` literal is
        // an ordinary argument, the function object the body receives. A call with only such
        // literals is therefore inlined like one without lambdas.
        if !self.ir.call_inline_modifiers.contains_key(&call_expression) {
            return InlineCallOutcome::NotApplicable;
        }
        let positions = match self.inlined_literal_positions(
            call_expression,
            leading_non_argument_operands,
            args,
        ) {
            Ok(positions) if positions.is_empty() => {
                return InlineCallOutcome::declined_or_handled(
                    self.try_inline_classpath_body(&inline_call, code),
                );
            }
            Ok(positions) => positions,
            Err(reason) => {
                self.run.set_inline_bail(reason);
                return InlineCallOutcome::HandledWithError;
            }
        };
        // The literal candidates are selected. Planning may still decline an unsupported host only
        // when every literal can legally become a real callable and the target itself is callable.
        // Otherwise this route owns the call and a failure is an error.
        // If the body INVOKES the lambda parameter (`FunctionN.invoke`), its lambda bodies replace
        // those invokes. If the lambda is used only as a VALUE, passed to the constructor of an
        // anonymous object the body creates (`Continuation(ctx){…}`'s
        // `new …$Continuation$1(ctx, resumeWith)`), the object is regenerated around it.
        let body_invokes_lambda =
            crate::jvm::inline::disassemble(&body.code).is_some_and(|insns| {
                !crate::jvm::inline::function_invoke_sites(&insns, &body.source_cp).is_empty()
            });
        if !body_invokes_lambda {
            return self.inline_value_used_lambda_call(&inline_call, code);
        }
        let reason = match self.lambda_call_route(&inline_call, code) {
            Ok(LambdaCallRoute::MethodInliner(callee)) => {
                return match self.inline_classpath_lambda_call(
                    &inline_call,
                    &callee,
                    LambdaPlacement::Invokes,
                    code,
                ) {
                    Ok(()) => InlineCallOutcome::Handled,
                    Err(reason) => {
                        self.run.set_inline_bail(reason);
                        InlineCallOutcome::HandledWithError
                    }
                };
            }
            Ok(LambdaCallRoute::Splice(reason)) => reason,
            Err(reason) => {
                self.run.set_inline_bail(reason);
                return InlineCallOutcome::HandledWithError;
            }
        };
        crate::trace_compiler!("splice", "literal-lambda call spliced: {reason:?}");
        self.splice_selected_literals(&inline_call, &positions, reason, base, code)
    }

    /// A call whose literal lambdas the callee's body uses only as values: each is passed to the
    /// constructor of an anonymous object the body creates (`Continuation(ctx){…}`'s
    /// `new …$Continuation$1(ctx, resumeWith)`), and the object is regenerated around it. The
    /// literals are selected, so the call is owned: a failure is an error, never a real call.
    pub(super) fn inline_value_used_lambda_call(
        &mut self,
        inline_call: &ClasspathInlineCall<'_, '_>,
        code: &mut CodeBuilder,
    ) -> InlineCallOutcome {
        // A shape the port does not own yet stays on the byte bridge. A body that constructs no
        // anonymous object takes the literal as an ordinary value (one passed where the parameter
        // is not function-typed, as `map[key] = { … }`).
        match self.lambda_call_route(inline_call, code) {
            Ok(LambdaCallRoute::MethodInliner(callee))
                if crate::jvm::inliner::constructs_anonymous_object(&callee, &self.bodies) =>
            {
                return match self.inline_classpath_lambda_call(
                    inline_call,
                    &callee,
                    LambdaPlacement::Objects,
                    code,
                ) {
                    Ok(()) => InlineCallOutcome::Handled,
                    Err(reason) => {
                        self.run.set_inline_bail(reason);
                        InlineCallOutcome::HandledWithError
                    }
                };
            }
            Ok(LambdaCallRoute::MethodInliner(_)) => {
                crate::trace_compiler!("splice", "value-used lambda bridged: no object");
            }
            Ok(LambdaCallRoute::Splice(reason)) => {
                crate::trace_compiler!("splice", "value-used lambda bridged: {reason:?}");
            }
            Err(reason) => {
                self.run.set_inline_bail(reason);
                return InlineCallOutcome::HandledWithError;
            }
        }
        if let Err(reason) = check_byte_splice_body(
            inline_call.target.name,
            inline_call.target.splice_desc,
            inline_call.body,
        ) {
            self.run.set_inline_bail(reason);
            return InlineCallOutcome::HandledWithError;
        }
        match self.try_inline_materialized_lambda_body(inline_call, code) {
            Some(()) => InlineCallOutcome::Handled,
            None => {
                self.run
                    .set_emit_error(VALUE_USED_LITERAL_UNSPLICED.to_string());
                InlineCallOutcome::HandledWithError
            }
        }
    }

    /// Temporary migration bridge for a literal lambda the inline body uses as a value (for
    /// example, the implementation object created by `Continuation(context, block)`). The
    /// MethodNode route will own this once anonymous-object regeneration lands. This operation is
    /// deliberately unavailable to no-lambda calls, so it cannot become their fallback again.
    pub(super) fn try_inline_materialized_lambda_body(
        &mut self,
        call: &ClasspathInlineCall<'_, '_>,
        code: &mut CodeBuilder,
    ) -> Option<()> {
        let ClasspathInlineCall {
            call_expression,
            target,
            args,
            leading_non_argument_operands,
            body,
            ..
        } = *call;
        let physical = parse_descriptor_params(target.splice_desc)?;
        if physical.len() != args.len() {
            return None;
        }
        // Every operand is on the stack before the body stores any parameter. A top run of holders
        // can therefore supply the inline frame only when each holder is already at that
        // parameter's exact slot; otherwise the stores could permute or overwrite a value that the
        // expanded caller still reads later.
        let parameters = args
            .iter()
            .zip(&physical)
            .map(|(&argument, &ty)| {
                let holder = match self.ir.expr(argument) {
                    IrExpr::GetValue(value) => Some(*value),
                    _ => None,
                };
                (holder, slot_words(ty))
            })
            .collect::<Vec<_>>();
        let base = self.inline_splice_base(self.frame.aligned_call_operand_base(&parameters));
        let frame = crate::jvm::inline::spliced_frame(body, target.splice_desc, &[], base)?;
        let probe =
            crate::jvm::inline::splice_unified(body, target.splice_desc, base, &[], 0, self.cw)?;
        if (!probe.handlers.is_empty() || !probe.external_branches.is_empty())
            && code.stack_height() != 0
        {
            return None;
        }
        self.emit_call_descriptor_operands(
            call_expression,
            leading_non_argument_operands,
            args,
            &physical,
            code,
        )
        .ok()?;
        let argument_words = physical.iter().map(|&ty| i32::from(slot_words(ty))).sum();
        let result_words = descriptor_ret_words(target.descriptor);
        let splice_start = code.bytes.len();
        let rewritten = if probe.needs_relayout {
            crate::jvm::inline::splice_unified(
                body,
                target.splice_desc,
                base,
                &[],
                splice_start,
                self.cw,
            )?
        } else {
            probe
        };
        bind_inline_handlers(code, &rewritten.handlers);
        let result_words = if rewritten.falls_through {
            result_words
        } else {
            0
        };
        code.splice_inline(
            &rewritten.bytes,
            &rewritten.external_branches,
            body.max_stack,
            frame.top_local,
            argument_words,
            result_words,
            rewritten.falls_through,
        );
        self.frame.reserve_through(frame.top_local);
        self.record_spliced_lines(
            &rewritten.lines,
            body,
            target.inline_only,
            if rewritten.needs_relayout {
                0
            } else {
                splice_start
            },
            code,
        );
        self.record_spliced_locals(
            &rewritten.locals,
            target.inline_only,
            if rewritten.needs_relayout {
                0
            } else {
                splice_start
            },
            code,
        );
        Some(())
    }

    /// The lines the caller's own source claims in its source map.
    fn claimable_lines(&self) -> u16 {
        u16::try_from(self.ir.source_line_count)
            .unwrap_or(u16::MAX)
            .max(1)
    }

    /// Where an inlined body's frame starts: kotlinc's frame size at the call (`frame_size`),
    /// above every live temporary. Not the method's `max_locals`: a block that ended before the
    /// call has handed its slots back, and the inlined body reuses them as kotlinc's does.
    fn inline_splice_base(&self, frame_size: u16) -> u16 {
        self.temporaries
            .live()
            .into_iter()
            .map(|(slot, ty)| slot + slot_words(ty))
            .fold(frame_size, u16::max)
    }

    /// Inline `target`, a call without literal lambda arguments its body invokes, through the
    /// ported inliner. `None` when this path does not cover a live call, which then stays a direct
    /// call; `Some(())` once the inlined code is written, or when the call is already unreachable
    /// and therefore needs no bytecode.
    pub(super) fn try_inline_classpath_body(
        &mut self,
        call: &ClasspathInlineCall<'_, '_>,
        code: &mut CodeBuilder,
    ) -> Option<()> {
        let ClasspathInlineCall {
            call_expression,
            target,
            args,
            leading_non_argument_operands,
            body,
            reified,
        } = *call;
        if code.is_dead() {
            return Some(());
        }
        let callee = match MethodNode::read(ACC_STATIC, target.name, target.splice_desc, body) {
            Ok(callee) => callee,
            Err(error) => {
                crate::trace_compiler!("splice", "unified inliner cannot read the body: {error:?}");
                return None;
            }
        };
        if let Some(shape) = inliner::unsupported_shape(&callee, &self.bodies) {
            crate::trace_compiler!("splice", "unified inliner declines {shape:?}");
            return None;
        }
        let physical = parse_descriptor_params(target.splice_desc)?;
        if physical.len() != args.len() {
            return None;
        }
        let supplies = self.parameter_supplies(
            call_expression,
            target,
            args,
            leading_non_argument_operands,
            &physical,
            &callee,
        );
        let Some(supplies) = supplies else {
            crate::trace_compiler!("splice", "unified inliner cannot bind the arguments");
            return None;
        };
        let parameters =
            self.call_parameters(&supplies, &physical, args, &HashMap::new(), Vec::new());
        let base = self.inline_splice_base(self.frame.size());
        // The body's lines are mapped once the arguments are evaluated, like kotlinc's, so that
        // the source map numbers them after any call inlined into an argument; this first pass only
        // settles whether the port covers the body.
        if let Err(error) = inliner::inline(
            &callee,
            &parameters,
            &[],
            target.inline_only,
            base,
            reified,
            inliner::InliningContext {
                lines: &mut UnmappedLines,
                classes: &self.bodies,
                objects: &mut self.call_objects(
                    call_expression,
                    target.name,
                    false,
                    SourceMap::default(),
                ),
            },
        ) {
            crate::trace_compiler!("splice", "unified inliner declines: {error:?}");
            return None;
        }
        let inlined_call = InlinedCall {
            call: *call,
            physical: &physical,
            supplies: &supplies,
        };
        let inlined = InlinedLambdas {
            lambdas: &[],
            lines: SourceMap::default(),
        };
        self.emit_inlined_call(&inlined_call, &callee, &parameters, inlined, base, code)
            .expect("the body was inlined before its arguments were evaluated");
        Some(())
    }

    /// Inline a call that route planning gave to the port ([`LambdaCallRoute::MethodInliner`]):
    /// each literal lambda its body invokes is compiled to a node and placed at those `invoke`s.
    /// An error is a broken invariant of that plan and fails the emission; the call is never
    /// retried as a splice.
    ///
    /// When the body only passes its lambdas to the anonymous objects it constructs
    /// (`LambdaPlacement::Objects`), their code is written into the regenerated objects alone, as
    /// kotlinc's is: what compiling them left in the caller's pool and source map is forgotten.
    pub(super) fn inline_classpath_lambda_call(
        &mut self,
        call: &ClasspathInlineCall<'_, '_>,
        callee: &MethodNode,
        placement: LambdaPlacement,
        code: &mut CodeBuilder,
    ) -> Result<(), &'static str> {
        let ClasspathInlineCall {
            call_expression,
            target,
            args,
            leading_non_argument_operands,
            ..
        } = *call;
        if code.is_dead() {
            return Ok(());
        }
        let physical = parse_descriptor_params(target.splice_desc)
            .ok_or("an inline callee's descriptor cannot be parsed")?;
        let supplies = self
            .parameter_supplies(
                call_expression,
                target,
                args,
                leading_non_argument_operands,
                &physical,
                callee,
            )
            .ok_or("a function-typed argument has no published crossinline/noinline modifier")?;
        let mut lambdas = Vec::new();
        let mut captured = Vec::new();
        let mut lambda_bindings = HashMap::new();
        let checkpoint = self.cw.checkpoint();
        for (index, &argument) in args.iter().enumerate() {
            if !self.is_inlined_literal(
                call_expression,
                leading_non_argument_operands,
                index,
                argument,
            )? {
                continue;
            }
            let argument = self.inline_lambda_node(argument, target.name)?;
            let start = captured.len();
            for &(capture, ty) in &argument.captures {
                captured.push(Parameter {
                    category: Category::of_descriptor(&type_descriptor(ty)),
                    binding: self.caller_local_binding(capture, ty),
                });
            }
            let mut lambda = argument.lambda;
            lambda.captured = start..captured.len();
            lambda_bindings.insert(index, lambdas.len());
            lambdas.push(lambda);
        }
        // The lambdas' lines are lines of the caller's map as compiling them left it.
        let lambda_lines = self
            .cw
            .source_map_for_inlining(self.claimable_lines())
            .cloned()
            .unwrap_or_default();
        // Route planning keeps every lambda that declares a member of the caller on the bridge;
        // one that still does is a broken plan, never code left in the caller.
        match placement {
            LambdaPlacement::Objects => {
                if !self.cw.rollback(checkpoint) {
                    return Err("an object-only lambda declared a member of the calling class");
                }
            }
            LambdaPlacement::Invokes => {
                // Compiling a literal lambda into its scratch MethodNode may itself inline calls
                // and allocate their source-map ranges. The scratch compile is not source
                // execution order: the enclosing callee maps its own body before it inserts the
                // lambda. Restore the caller's map now; `CallLines::lambda` imports the saved
                // lambda map at the insertion.
                self.cw.restore_source_map(&checkpoint);
            }
        }
        let parameters =
            self.call_parameters(&supplies, &physical, args, &lambda_bindings, captured);
        let base = self.inline_splice_base(self.frame.size());
        let inlined_call = InlinedCall {
            call: *call,
            physical: &physical,
            supplies: &supplies,
        };
        let inlined = InlinedLambdas {
            lambdas: &lambdas,
            lines: lambda_lines,
        };
        self.emit_inlined_call(&inlined_call, callee, &parameters, inlined, base, code)
            .map_err(|error| {
                crate::trace_compiler!("splice", "the inliner rejected a planned call: {error:?}");
                "the inliner rejected a call its route planning gave it"
            })
    }

    /// Each parameter's binding: a lambda the callee invokes, a caller local, or a temporary.
    fn call_parameters(
        &self,
        supplies: &[Supply],
        physical: &[Ty],
        args: &[u32],
        lambda_bindings: &HashMap<usize, usize>,
        captured: Vec<Parameter>,
    ) -> Parameters {
        Parameters {
            parameters: supplies
                .iter()
                .zip(physical)
                .zip(args)
                .enumerate()
                .map(|(index, ((supply, &ty), &argument))| Parameter {
                    category: Category::of_descriptor(&type_descriptor(ty)),
                    binding: match (lambda_bindings.get(&index), supply) {
                        (Some(&lambda), _) => Binding::Lambda(lambda),
                        (None, Supply::CallerLocal) => self.caller_local_binding(argument, ty),
                        (None, Supply::Stored | Supply::InPlace) => Binding::Temporary,
                    },
                })
                .collect(),
            captured,
        }
    }

    /// Evaluate the call's arguments into their temporaries, then write the inlined body.
    fn emit_inlined_call(
        &mut self,
        call: &InlinedCall<'_, '_>,
        callee: &MethodNode,
        parameters: &Parameters,
        inlined_lambdas: InlinedLambdas<'_>,
        base: u16,
        code: &mut CodeBuilder,
    ) -> Result<(), InlineError> {
        let InlinedLambdas {
            lambdas,
            lines: lambda_lines,
        } = inlined_lambdas;
        let InlinedCall {
            call:
                ClasspathInlineCall {
                    call_expression,
                    target,
                    args,
                    leading_non_argument_operands,
                    body,
                    reified,
                },
            physical,
            supplies,
        } = *call;
        let temporaries = parameters.temporaries();

        // Arguments, in order: each is stored right after it is evaluated.
        let argument_frame = self.frame.mark();
        self.frame.reserve_through(base);
        let origins = self.default_operand_origins(call_expression, args, false);
        let mut inside_run = false;
        let mut leases = Vec::new();
        let mut in_place = HashMap::new();
        for (index, _) in args.iter().enumerate() {
            let Some(temporary) = temporaries[index] else {
                continue;
            };
            let slot = base + temporary;
            if supplies[index] == Supply::InPlace {
                in_place.insert(slot, index);
                continue;
            }
            self.frame.reserve_through(slot);
            let operand_frame = self.frame.mark();
            self.mark_synthesized_operand_run(
                Some((call_expression, &origins)),
                index,
                &mut inside_run,
                code,
            );
            self.emit_call_operand(
                call_expression,
                leading_non_argument_operands,
                args,
                physical,
                index,
                code,
            );
            self.frame.rewind_to(operand_frame);
            let entered = self
                .frame
                .enter_temp(TempRole::InlineArgument, physical[index]);
            debug_assert_eq!(entered.slot(), slot);
            store(physical[index], slot, code);
            leases.push(self.lease_frame_temporary(entered, physical[index]));
        }

        // Like any call, an inline call states its line once its arguments are evaluated; that line
        // is the call site the body's lines are mapped against.
        self.mark_expression_start(call_expression, code);
        let caller_line = code.current_line();
        let call_line = caller_line.unwrap_or(1);
        // An `@InlineOnly` body whose ordinary arguments are movable evaluates them where the
        // copied body first loads their temporary. The eager expression boundary above exists so
        // the call line can anchor the body's source-map interval, but it must not claim the
        // generated prefix before that first source argument. Withdrawing it leaves the enclosing
        // inline line in effect until emitting the in-place operand marks the call-site line.
        if supplies.contains(&Supply::InPlace) {
            code.withdraw_line();
        }
        let claimable = self.claimable_lines();
        let bodies = self.bodies;
        let mut lines = CallLines {
            emitter: self,
            body,
            inline_only: target.inline_only,
            call_line,
            claimable,
            visited: HashMap::new(),
            lambda_lines: &lambda_lines,
            lambda_visited: HashMap::new(),
        };
        let mut objects =
            lines
                .emitter
                .call_objects(call_expression, target.name, true, lambda_lines.clone());
        let inlined = inliner::inline(
            callee,
            parameters,
            lambdas,
            target.inline_only,
            base,
            reified,
            inliner::InliningContext {
                lines: &mut lines,
                classes: &bodies,
                objects: &mut objects,
            },
        );
        let inlined = match inlined {
            Ok(inlined) => inlined,
            Err(error) => {
                for lease in leases {
                    self.release_temporary(lease);
                }
                self.frame.rewind_to(argument_frame);
                return Err(error);
            }
        };
        let placement = InlinePlacement {
            call_expression,
            args,
            leading_non_argument_operands,
            physical,
            in_place,
            origins: &origins,
        };
        // kotlinc's `inlineCall`: a body with a loop or a try/catch (`requiresEmptyStackOnEntry`)
        // is bracketed by `beforeInlineCall`/`afterInlineCall`, and FixStack saves whatever the
        // caller already pushed into locals there, when the class is written. kotlinc brackets
        // every such body; around an empty stack FixStack only removes the markers, so they are
        // written where there is something to save.
        let spills_operands =
            inliner::requires_empty_stack_on_entry(callee) && code.stack_height() != 0;
        if spills_operands {
            crate::trace_compiler!(
                "splice",
                "{}.{} saves {} operand word(s) around its body",
                target.owner,
                target.name,
                code.stack_height()
            );
            code.inline_call_marker(true);
        }
        self.write_inlined_node(&inlined, placement, code);
        if spills_operands {
            code.inline_call_marker(false);
        }
        // kotlinc's `markLineNumberAfterInlineIfNeeded`: inside a condition the caller's line is
        // marked again for the jump that follows; elsewhere it is forgotten, so the next mark of
        // any line is written.
        match caller_line {
            Some(line)
                if self.inside_condition
                    || self
                        .regeneration_site
                        .as_ref()
                        .is_some_and(RegenerationSite::is_class_initializer) =>
            {
                code.inlined_line(line)
            }
            _ => code.forget_line(),
        }

        for lease in leases {
            self.release_temporary(lease);
        }
        self.frame.rewind_to(argument_frame);
        Ok(())
    }

    /// A source lambda with an inline body, or an unbound constructor reference the inliner places
    /// as that lambda.
    fn is_inline_lambda_shape(&self, argument: u32) -> bool {
        matches!(
            self.ir.expr(argument),
            IrExpr::Lambda {
                inline_body: Some(_),
                ..
            }
        ) || callable_reference_template(self.ir, argument).is_some()
    }

    /// Whether argument `index` of `call_expression` is a literal lambda the inline body's invokes
    /// expand: one passed to an inline parameter ([`super::inline_parameters`]). A literal for a
    /// `noinline` or non-function parameter is an ordinary argument, the function object the body
    /// receives. An error is a literal the call published no parameter facts for.
    pub(super) fn is_inlined_literal(
        &self,
        call_expression: u32,
        leading_non_argument_operands: usize,
        index: usize,
        argument: u32,
    ) -> Result<bool, &'static str> {
        if !self.is_inline_lambda_shape(argument) {
            return Ok(false);
        }
        index
            .checked_sub(leading_non_argument_operands)
            .and_then(|parameter| {
                super::inline_parameters::is_inline_parameter(self.ir, call_expression, parameter)
            })
            .ok_or("a lambda argument has no published parameter modifier and declared type")
    }

    /// The positions of `args` holding a literal the inline body's invokes expand, by
    /// [`Self::is_inlined_literal`].
    pub(super) fn inlined_literal_positions(
        &self,
        call_expression: u32,
        leading_non_argument_operands: usize,
        args: &[u32],
    ) -> Result<Vec<usize>, &'static str> {
        let mut positions = Vec::new();
        for (index, &argument) in args.iter().enumerate() {
            if self.is_inlined_literal(
                call_expression,
                leading_non_argument_operands,
                index,
                argument,
            )? {
                positions.push(index);
            }
        }
        Ok(positions)
    }

    /// kotlinc's choice per argument. An `@InlineOnly` callee reads a dispatch receiver or ordinary
    /// argument that is already a local from that local (`genOrGetLocal`), including a local under
    /// a representation-preserving reference coercion; its extension receiver is always stored.
    /// When the body loads its parameters first, the stored arguments are instead evaluated in
    /// place.
    ///
    /// `None` only when a function-typed local has no provider-published `crossinline`/`noinline`
    /// modifier. An inline parameter, `crossinline` included, reads that local where it already
    /// lives; a `noinline` parameter is stored like an ordinary value.
    fn parameter_supplies(
        &self,
        call_expression: u32,
        target: &InlineStaticTarget<'_>,
        args: &[u32],
        leading_non_argument_operands: usize,
        physical: &[Ty],
        callee: &MethodNode,
    ) -> Option<Vec<Supply>> {
        let function_typed = |argument: u32| {
            let semantic = self
                .ir
                .logical_types
                .get(&argument)
                .copied()
                .unwrap_or_else(|| self.value_ty(argument));
            matches!(semantic.non_null(), Ty::Fun(_))
        };
        let in_place = target.inline_only
            && !args.is_empty()
            && !args.iter().any(|&argument| function_typed(argument))
            && inliner::can_inline_arguments_in_place(callee)
            && in_place_arguments::arguments_movable(self.ir, args, &|construction| {
                !self
                    .sam_wrapper_realizations
                    .is_nullable_construction(construction)
            });
        let mut supplies = Vec::with_capacity(args.len());
        for (index, &argument) in args.iter().enumerate() {
            let evaluated = if in_place {
                Supply::InPlace
            } else {
                Supply::Stored
            };
            // kotlinc never stores an inline suspend callee's continuation: the body reads the
            // caller's own continuation local, as every suspension point in the caller does.
            if self.reads_continuation_local(argument) {
                supplies.push(Supply::CallerLocal);
                continue;
            }
            let Some(operand) = self.reference_local_operand(argument) else {
                supplies.push(evaluated);
                continue;
            };
            if !self.local_needs_no_boxing(operand, physical[index]) {
                supplies.push(evaluated);
                continue;
            }
            if !target.inline_only {
                supplies.push(evaluated);
                continue;
            }
            let extension_receiver = self
                .ir
                .static_extension_receivers
                .get(&call_expression)
                .is_some_and(|&position| {
                    position as usize + leading_non_argument_operands == index
                });
            if extension_receiver {
                supplies.push(evaluated);
                continue;
            }
            if function_typed(operand) {
                let parameter = index.checked_sub(leading_non_argument_operands)?;
                let inlining = self
                    .ir
                    .call_inline_modifiers
                    .get(&call_expression)?
                    .get(parameter)
                    .copied()?;
                supplies.push(
                    if inlining == crate::types::InlineParameterModifier::Noinline {
                        evaluated
                    } else {
                        Supply::CallerLocal
                    },
                );
                continue;
            }
            supplies.push(Supply::CallerLocal);
        }
        Some(supplies)
    }

    /// The caller local an argument reads, through coercions that keep the reference as it is.
    ///
    /// `println(message)` widens `String` to `Any?`. That coercion writes no bytecode, so kotlinc's
    /// `genOrGetLocal` still loads the caller's local. A coercion that boxes or unboxes (`Int` to
    /// `Any`) is not this local: the parameter is a different representation.
    fn reference_local_operand(&self, argument: u32) -> Option<u32> {
        let mut current = argument;
        loop {
            match self.ir.expr(current) {
                IrExpr::GetValue(value) if self.slots.contains_key(value) => return Some(current),
                IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    arg,
                    type_operand,
                } if self.reference_view_coercion(*arg, *type_operand) => current = *arg,
                _ => return None,
            }
        }
    }

    /// Whether an implicit coercion leaves a reference value in the same JVM representation.
    fn reference_view_coercion(&self, arg: u32, type_operand: Ty) -> bool {
        let source = self.value_ty(arg);
        let target = ir_ty_to_jvm(&stored_value_ty(type_operand));
        source.is_reference()
            && target.is_reference()
            && source.scalar_value_repr().is_none()
            && type_operand.scalar_value_repr().is_none()
    }

    /// kotlinc's `isLocalWithNoBoxing`: the local and the parameter are both primitive or both
    /// references, and no value class is boxed or unboxed between them.
    fn local_needs_no_boxing(&self, argument: u32, parameter: Ty) -> bool {
        let IrExpr::GetValue(value) = self.ir.expr(argument) else {
            return false;
        };
        let Some(&(_, local)) = self.slots.get(value) else {
            return false;
        };
        let primitive = |ty: Ty| !ty.is_reference() && slot_words(ty) > 0;
        if primitive(local) || primitive(parameter) {
            return local == parameter;
        }
        local.scalar_value_repr().is_none() && parameter.scalar_value_repr().is_none()
    }

    /// Whether `argument` is the continuation this emission reads from its own local, rather than
    /// a fake continuation or one the CPS pass bound to a value.
    fn reads_continuation_local(&self, argument: u32) -> bool {
        matches!(self.ir.expr(argument), IrExpr::CurrentContinuation)
            && self.continuation_slot.is_some()
            && !self.is_fake_continuation(argument)
    }

    /// The caller local an argument is read from, coerced to the parameter's class as kotlinc's
    /// `StackValue.coerce` does between two reference types.
    fn caller_local_binding(&self, argument: u32, parameter: Ty) -> Binding {
        if self.reads_continuation_local(argument) {
            let slot = self
                .continuation_slot
                .expect("a continuation read binds the continuation slot");
            return Binding::CallerLocal {
                slot,
                category: Category::Reference,
                checkcast: None,
            };
        }
        let argument = self.reference_local_operand(argument).unwrap_or(argument);
        let IrExpr::GetValue(value) = self.ir.expr(argument) else {
            unreachable!("a caller-local argument is a local read");
        };
        let (slot, local) = self.slots[value];
        let category = Category::of_descriptor(&type_descriptor(local));
        let checkcast = (category == Category::Reference
            && !crate::jvm::names::same_type_descriptor(local, parameter))
        .then(|| checkcast_internal(parameter))
        .flatten();
        Binding::CallerLocal {
            slot,
            category,
            checkcast,
        }
    }

    /// Evaluate one call operand and adapt it to its descriptor type, as
    /// [`Self::emit_call_descriptor_operands`] does for every operand in turn.
    fn emit_call_operand(
        &mut self,
        call_expression: u32,
        leading_non_argument_operands: usize,
        args: &[u32],
        physical: &[Ty],
        index: usize,
        code: &mut CodeBuilder,
    ) {
        let expression = args[index];
        self.emit_value(expression, code);
        let source = self.value_ty(expression);
        match index.checked_sub(leading_non_argument_operands) {
            Some(argument_index) => self.adapt_physical_call_operand_for(
                call_expression,
                argument_index,
                expression,
                source,
                physical[index],
                code,
            ),
            None => self.adapt_physical_operand_for(expression, source, physical[index], code),
        }
    }

    /// Write an inlined node into the caller: labels become caller labels, jumps and switches are
    /// linked through them, every other instruction is encoded against the caller's pool, line
    /// numbers are mapped into the caller's source map, and an in-place argument is evaluated where
    /// the body loads its temporary.
    fn write_inlined_node(
        &mut self,
        inlined: &MethodNode,
        mut placement: InlinePlacement<'_>,
        code: &mut CodeBuilder,
    ) {
        let shapes = stack_shapes(inlined).expect("an inlined body's stack was already followed");
        let baseline = code.stack_height();
        let labels: Vec<Label> = (0..inlined.label_count())
            .map(|_| code.new_label())
            .collect();
        let mut bound_at = vec![None; labels.len()];
        for block in &inlined.try_catch_blocks {
            let catch_type = block
                .catch_type
                .as_deref()
                .map_or(0, |class| self.cw.class_ref(class));
            code.add_exception(
                labels[block.start.index()],
                labels[block.end.index()],
                labels[block.handler.index()],
                catch_type,
            );
        }
        let mut top_local = 0u16;
        let mut duplicating: Option<(u8, u16)> = None;
        // The expanded lambdas' `beforeInlineCall`/`afterInlineCall` brackets that are written:
        // those with operands to save, the caller's or the body's own. FixStack stores them into
        // locals when the class is written, so inside a written bracket the stack holds only what
        // the lambda pushes, as `stack_shapes` follows it.
        let mut brackets: Vec<bool> = Vec::new();
        let under = |brackets: &[bool]| {
            if brackets.contains(&true) {
                0
            } else {
                baseline
            }
        };
        for (at, node) in inlined.nodes.iter().enumerate() {
            match node {
                Node::Label(label) => {
                    let caller = labels[label.index()];
                    match &shapes[at] {
                        Some(stack) if code.is_dead() => {
                            code.bind_external_target(caller);
                            code.set_stack_height(under(&brackets) + words(stack));
                        }
                        Some(stack) => {
                            code.bind(caller);
                            code.set_stack_height(under(&brackets) + words(stack));
                        }
                        None => code.bind(caller),
                    }
                    bound_at[label.index()] = Some(code.bytes.len());
                }
                Node::Line { line, .. } => code.inlined_line(*line),
                Node::Insn(_) if is_before_inline_marker(node) => {
                    let saves = shapes[at]
                        .as_ref()
                        .is_some_and(|stack| under(&brackets) + words(stack) > 0);
                    if saves {
                        code.inline_call_marker(true);
                        code.set_stack_height(0);
                    }
                    brackets.push(saves);
                }
                Node::Insn(_) if is_after_inline_marker(node) => {
                    if brackets.pop().unwrap_or(false) {
                        code.inline_call_marker(false);
                        if let Some(stack) = shapes.get(at + 1).cloned().flatten() {
                            code.set_stack_height(under(&brackets) + words(&stack));
                        }
                    }
                }
                Node::Insn(insn) => {
                    if let Insn::Var { op, slot } = insn {
                        top_local = top_local.max(slot + var_words(*op));
                        if (0x15..=0x19).contains(op) {
                            if duplicating == Some((*op, *slot)) {
                                code.instruction(
                                    &[if var_words(*op) == 2 { 0x5c } else { 0x59 }],
                                    var_words(*op) as i32,
                                    false,
                                );
                                continue;
                            }
                            if let Some(index) = placement.in_place.remove(slot) {
                                self.emit_in_place_argument(&placement, index, *slot, code);
                                duplicating = Some((*op, *slot));
                                continue;
                            }
                        }
                    }
                    duplicating = None;
                    if let Insn::Iinc { slot, .. } = insn {
                        top_local = top_local.max(slot + 1);
                    }
                    self.write_inlined_instruction(insn, &labels, code);
                }
            }
        }
        code.ensure_locals(top_local);
        if !self.record_locals {
            return;
        }
        for local in &inlined.local_variables {
            let (Some(start), Some(end)) =
                (bound_at[local.start.index()], bound_at[local.end.index()])
            else {
                continue;
            };
            let (Ok(start), Ok(length)) = (
                u16::try_from(start),
                u16::try_from(end.saturating_sub(start)),
            ) else {
                continue;
            };
            code.add_local_entry(start, Some(length), local.slot, &local.name, &local.desc);
        }
    }

    fn write_inlined_instruction(&mut self, insn: &Insn, labels: &[Label], code: &mut CodeBuilder) {
        let delta = word_delta(insn).expect("an inlined body's stack was already followed");
        match insn {
            Insn::Jump { op, target } => code.jump(*op, labels[target.index()], delta),
            Insn::TableSwitch {
                low,
                high,
                default,
                labels: targets,
            } => {
                let targets: Vec<Label> = targets.iter().map(|t| labels[t.index()]).collect();
                code.tableswitch(*low, *high, labels[default.index()], &targets);
            }
            Insn::LookupSwitch {
                default,
                keys,
                labels: targets,
            } => {
                let pairs: Vec<(i32, Label)> = keys
                    .iter()
                    .zip(targets)
                    .map(|(&key, t)| (key, labels[t.index()]))
                    .collect();
                code.lookupswitch(labels[default.index()], &pairs);
            }
            _ => {
                let bytes = self
                    .cw
                    .copying(|cw| encode_instruction(insn, cw))
                    .ok()
                    .flatten()
                    .expect("every non-branch instruction has a fixed encoding");
                code.instruction(&bytes, delta, is_terminal(insn));
            }
        }
    }

    fn emit_in_place_argument(
        &mut self,
        placement: &InlinePlacement<'_>,
        index: usize,
        temporary: u16,
        code: &mut CodeBuilder,
    ) {
        // kotlinc evaluated the argument before the body, with the earlier arguments' temporaries
        // already entered into its frame, and moved the code here afterwards.
        let mut inside_run = false;
        let saved = self.frame.mark();
        self.frame.reserve_through(temporary);
        self.mark_synthesized_operand_run(
            Some((placement.call_expression, placement.origins)),
            index,
            &mut inside_run,
            code,
        );
        self.emit_call_operand(
            placement.call_expression,
            placement.leading_non_argument_operands,
            placement.args,
            placement.physical,
            index,
            code,
        );
        self.frame.rewind_to(saved);
    }
}

impl Emitter<'_> {
    /// Map a line of an inlined body into the caller's source map (kotlinc's `SourceMapCopier`):
    /// through the dependency's own map when it has one, then as a range of the caller's
    /// `SMAP`. `None` for a line nothing maps: an `@InlineOnly` body's, or one whose file the
    /// dependency does not name.
    pub(super) fn map_inlined_line(
        &mut self,
        body: &crate::jvm::classreader::MethodCode,
        inline_only: bool,
        line: u16,
        call_line: u16,
        claimable: u16,
    ) -> Option<u16> {
        if inline_only {
            return None;
        }
        let source_file = body.source_file.as_deref()?;
        // The path a line is recorded under is the class the code was READ from — for a multifile
        // facade's function, the part class that holds its body, not the facade the call names.
        let path = body.defining_class.as_str();
        let (name, path, source) = match body.dependency_source_map.as_ref() {
            Some(map) => map.resolve(line)?,
            None => (source_file, path, line),
        };
        self.cw
            .source_map_for_inlining(claimable)
            .and_then(|map| map.map_line(name, path, source, call_line))
    }
}

/// Lines left as the body's own, for the pass that only checks the body can be inlined.
struct UnmappedLines;

impl inliner::SourceLines for UnmappedLines {
    fn map(&mut self, line: u16) -> Option<u16> {
        Some(line)
    }
    fn synthetic(&mut self) -> Option<u16> {
        None
    }
    fn call_site(&self) -> Option<u16> {
        None
    }
}

/// The inlined body's lines, mapped into the caller's source map against the call's line. A line
/// the body revisits keeps the line it was first mapped to (`SourceMapCopier.visitedLines`), even
/// when a range mapped since would extend to cover it.
struct CallLines<'e, 'a, 'b, 'l> {
    emitter: &'e mut Emitter<'a>,
    body: &'b crate::jvm::classreader::MethodCode,
    inline_only: bool,
    call_line: u16,
    claimable: u16,
    visited: HashMap<u16, u16>,
    /// Source map produced while compiling the call's literal lambdas into scratch methods.
    lambda_lines: &'l SourceMap,
    /// One `SourceMapCopier` for those lambda lines: each old output line is imported once.
    lambda_visited: HashMap<u16, u16>,
}

impl inliner::SourceLines for CallLines<'_, '_, '_, '_> {
    fn map(&mut self, line: u16) -> Option<u16> {
        if let Some(&mapped) = self.visited.get(&line) {
            return Some(mapped);
        }
        let mapped = self.emitter.map_inlined_line(
            self.body,
            self.inline_only,
            line,
            self.call_line,
            self.claimable,
        )?;
        self.visited.insert(line, mapped);
        Some(mapped)
    }
    fn synthetic(&mut self) -> Option<u16> {
        self.emitter
            .cw
            .source_map_for_inlining(self.claimable)
            .and_then(|map| map.map_synthetic_line(1))
    }
    fn call_site(&self) -> Option<u16> {
        Some(self.call_line)
    }

    fn lambda(&mut self, line: u16) -> Option<u16> {
        if let Some(&mapped) = self.lambda_visited.get(&line) {
            return Some(mapped);
        }
        let (name, path, source, call_site) = self.lambda_lines.resolve(line)?;
        let mapped = self
            .emitter
            .cw
            .source_map_for_inlining(self.claimable)?
            .map_copied_line(name, path, source, call_site)?;
        self.lambda_visited.insert(line, mapped);
        Some(mapped)
    }
}

/// Where an inlined body places its lambdas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LambdaPlacement {
    /// The body invokes some of them, so their code lands in the caller.
    Invokes,
    /// The body only passes them to the anonymous objects it constructs, whose copies inline them.
    Objects,
}

/// The compiled lambdas of a call, and the source map their lines are lines of.
struct InlinedLambdas<'a> {
    lambdas: &'a [inliner::Lambda],
    lines: SourceMap,
}

/// What writing the inlined node needs to know about the call's arguments.
/// One call to a classpath inline function, as the call dispatch found it.
#[derive(Clone, Copy)]
pub(super) struct ClasspathInlineCall<'a, 't> {
    pub call_expression: u32,
    pub target: &'a InlineStaticTarget<'t>,
    pub args: &'a [u32],
    pub leading_non_argument_operands: usize,
    pub body: &'a crate::jvm::classreader::MethodCode,
    pub reified: &'a crate::jvm::reified_arguments::ReifiedArguments,
}

/// A call on its way through the port, with what the call site decided per parameter.
#[derive(Clone, Copy)]
struct InlinedCall<'a, 't> {
    call: ClasspathInlineCall<'a, 't>,
    physical: &'a [Ty],
    supplies: &'a [Supply],
}

struct InlinePlacement<'a> {
    call_expression: u32,
    args: &'a [u32],
    leading_non_argument_operands: usize,
    physical: &'a [Ty],
    /// An in-place argument's temporary → its operand index; removed once evaluated.
    in_place: HashMap<u16, usize>,
    origins: &'a [crate::jvm::default_call_operands::DefaultOperandOrigin],
}

fn words(stack: &[Category]) -> i32 {
    stack.iter().map(|category| category.words()).sum()
}

fn var_words(op: u8) -> u16 {
    if matches!(op, 0x16 | 0x18 | 0x37 | 0x39) {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::classreader::{MethodCode, C};

    /// A static `()V` body over a pool that names `Intrinsics.reifiedOperationMarker(ILjava/lang/String;)V`
    /// at index 6 and the string `T` at index 8.
    fn body(code: Vec<u8>) -> MethodCode {
        MethodCode {
            max_stack: 2,
            max_locals: 0,
            code,
            source_cp: vec![
                C::Other,
                C::Utf8("kotlin/jvm/internal/Intrinsics".to_string()),
                C::Class(1),
                C::Utf8("reifiedOperationMarker".to_string()),
                C::Utf8("(ILjava/lang/String;)V".to_string()),
                C::NameAndType(3, 4),
                C::Methodref(2, 5),
                C::Utf8("T".to_string()),
                C::String(7),
            ]
            .into(),
            stackmap: None,
            handlers: Vec::new(),
            locals: Vec::new(),
            lines: Vec::new(),
            source_file: None,
            defining_class: "Test".to_string(),
            dependency_source_map: None,
            bootstrap_methods: Vec::new(),
        }
    }

    #[test]
    fn a_plain_body_may_take_the_byte_splice() {
        let plain = body(vec![0xb1]);
        assert_eq!(check_byte_splice_body("plain", "()V", &plain), Ok(()));
    }

    /// `iconst_1; ldc "T"; invokestatic reifiedOperationMarker; return`.
    #[test]
    fn a_reified_body_never_reaches_the_byte_splice() {
        let reified = body(vec![0x04, 0x12, 8, 0xb8, 0, 6, 0xb1]);
        assert_eq!(
            check_byte_splice_body("reified", "()V", &reified),
            Err(REIFIED_BODY_ON_BYTE_SPLICE)
        );
    }

    /// A `bipush` cut off before its operand: the reader's failure is the refusal, not a pass.
    #[test]
    fn an_unreadable_body_never_reaches_the_byte_splice() {
        let truncated = body(vec![0x10]);
        assert_eq!(
            check_byte_splice_body("truncated", "()V", &truncated),
            Err(UNREADABLE_INLINE_BODY)
        );
    }

    /// A facade names the call. The body, and the private bit, belong to the part class.
    struct FacadePart {
        part: &'static str,
        part_is_private: bool,
    }

    impl crate::jvm::inline::MethodBodies for FacadePart {
        fn body(&self, owner: &str, _name: &str, _descriptor: &str) -> Option<MethodCode> {
            let mut code = body(vec![0xb1]);
            code.defining_class = if owner == "kotlin/collections/MapsKt" {
                self.part.to_string()
            } else {
                owner.to_string()
            };
            Some(code)
        }

        fn member_is_private(&self, owner: &str, _name: &str, _descriptor: &str) -> bool {
            owner == self.part && self.part_is_private
        }
    }

    #[test]
    fn a_private_part_method_behind_a_facade_is_inline_only() {
        let bodies = FacadePart {
            part: "kotlin/collections/MapsKt__MapsKt",
            part_is_private: true,
        };
        assert!(declaration_is_inline_only(
            &bodies,
            "kotlin/collections/MapsKt",
            "set",
            "(Ljava/util/Map;Ljava/lang/Object;Ljava/lang/Object;)V",
            crate::libraries::InlineKind::MustInline,
        ));
    }

    #[test]
    fn a_public_reified_method_on_its_owner_is_not_inline_only() {
        let bodies = FacadePart {
            part: "kotlin/collections/MapsKt__MapsKt",
            part_is_private: true,
        };
        assert!(!declaration_is_inline_only(
            &bodies,
            "kotlin/sequences/SequencesKt",
            "filterIsInstance",
            "(Lkotlin/sequences/Sequence;)Lkotlin/sequences/Sequence;",
            crate::libraries::InlineKind::MustInline,
        ));
    }

    #[test]
    fn a_public_part_method_behind_a_facade_is_not_inline_only() {
        let bodies = FacadePart {
            part: "kotlin/collections/MapsKt__MapsKt",
            part_is_private: false,
        };
        assert!(!declaration_is_inline_only(
            &bodies,
            "kotlin/collections/MapsKt",
            "emptyMap",
            "()Ljava/util/Map;",
            crate::libraries::InlineKind::MustInline,
        ));
    }

    #[test]
    fn an_optional_inline_method_is_not_inline_only() {
        let bodies = FacadePart {
            part: "kotlin/collections/MapsKt__MapsKt",
            part_is_private: true,
        };
        assert!(!declaration_is_inline_only(
            &bodies,
            "kotlin/collections/MapsKt",
            "set",
            "(Ljava/util/Map;Ljava/lang/Object;Ljava/lang/Object;)V",
            crate::libraries::InlineKind::CanInline,
        ));
    }
}
