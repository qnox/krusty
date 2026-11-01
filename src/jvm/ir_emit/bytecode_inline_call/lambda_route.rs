//! Which lowering a call to a classpath inline function with literal lambda arguments takes. The
//! route is decided from checked facts before any of the call's code is emitted and is never
//! retried as the other: the MethodInliner port owns every shape it covers, and the byte splice
//! keeps, by name, the lambda and callee shapes later stages of the port take over
//! ([`SpliceReason`]).

use super::*;
use crate::jvm::inliner::UnsupportedShape;

/// The lowering of one inline call with literal lambda arguments.
pub(in crate::jvm::ir_emit) enum LambdaCallRoute {
    /// kotlinc's `MethodInliner` over the callee's node, with the omitted parameters whose default
    /// lambdas it inlines.
    MethodInliner(MethodNode, Vec<inliner::ExtractedDefaultLambda>),
    /// The byte splice, for a shape the port does not own yet.
    Splice(SpliceReason),
}

/// A shape the byte splice still owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::jvm::ir_emit) enum SpliceReason {
    /// A lambda suspends. The splice places its state-machine markers until the coroutine
    /// transformer runs on inlined bytecode.
    SuspendingLambda,
    /// A lambda's parameter or result crosses `invoke` through a value-class adapter rather than a
    /// plain box.
    ValueClassAdapter,
    /// The callee needs a later stage of the port.
    CalleeShape(UnsupportedShape),
    /// An `@InlineOnly` callee takes its arguments in place, beside a lambda.
    InPlaceArguments,
    /// A lambda captures a caller local whose type is not the captured value's.
    CaptureOutsideFrame,
    /// A lambda the callee may pass to an object it creates reaches a private declaration, which
    /// kotlinc reaches from the regenerated object through synthetic accessors.
    PrivateMemberInObject,
    /// An object the callee constructs reads a suspend lambda of the call from a method without
    /// the `$$forInline` copy kotlinc writes for it
    /// ([`inliner::suspend_lambda_without_for_inline_copy`]): a library krusty compiled, which
    /// writes no such copy yet.
    SuspendMethodWithoutForInlineCopy,
}

impl Emitter<'_> {
    /// The route of `call`, whose callee's body invokes at least one of the literal lambdas among
    /// its arguments. A literal for a `noinline` parameter is an ordinary stored argument on either
    /// route. An error is a fact the checked IR or the callee's class file should have made
    /// impossible.
    pub(in crate::jvm::ir_emit) fn lambda_call_route(
        &mut self,
        call: &ClasspathInlineCall<'_, '_>,
    ) -> Result<LambdaCallRoute, &'static str> {
        let ClasspathInlineCall {
            call_expression,
            target,
            args,
            leading_non_argument_operands,
            ..
        } = *call;
        let CalleeNodes {
            declared,
            callee,
            default_lambdas,
        } = self.inline_callee_nodes(call)?;
        // Only the MethodNode inliner specializes reified type parameters, so a reified body in a
        // shape the port does not own yet fails cleanly instead of taking the splice.
        let reified_body = inliner::has_reified_markers(&callee);
        let splice = |reason| {
            if reified_body {
                Err(REIFIED_BODY_ON_BYTE_SPLICE)
            } else {
                Ok(LambdaCallRoute::Splice(reason))
            }
        };
        let physical = parse_descriptor_params(target.splice_desc)
            .filter(|physical| physical.len() == args.len())
            .ok_or("an inline callee's descriptor does not match the call's operands")?;
        let mut lambda_arguments = Vec::new();
        for (index, &argument) in args.iter().enumerate() {
            if !self.is_inlined_literal(
                call_expression,
                leading_non_argument_operands,
                index,
                argument,
            )? {
                continue;
            }
            if let Some(reason) = self.lambda_splice_reason(argument)? {
                return splice(reason);
            }
            lambda_arguments.push(argument);
        }
        if let Some(shape) = inliner::unsupported_shape(&callee) {
            return splice(SpliceReason::CalleeShape(shape));
        }
        let supplies = self
            .parameter_supplies(call, &physical, &callee, &declared)
            .ok_or("a function-typed argument has no published crossinline/noinline modifier")?;
        if supplies.contains(&Supply::InPlace) {
            return splice(SpliceReason::InPlaceArguments);
        }
        for &argument in &lambda_arguments {
            if !self.lambda_captures_bind(argument)? {
                return splice(SpliceReason::CaptureOutsideFrame);
            }
        }
        // Whether the call's objects regenerate is settled from the callee's facts before any code
        // is emitted; once routed here, a failure to regenerate is an error, never another route.
        if inliner::constructs_regenerated_object(&callee, &self.bodies) {
            for &argument in &lambda_arguments {
                if self.lambda_reaches_private_members(argument)? {
                    return splice(SpliceReason::PrivateMemberInObject);
                }
            }
            let mut lambdas = Vec::new();
            let mut captured = Vec::new();
            let mut lambda_bindings = HashMap::default();
            for (index, &argument) in args.iter().enumerate() {
                if !lambda_arguments.contains(&argument) {
                    continue;
                }
                let (mut lambda, captures) = self.lambda_shape(argument)?;
                let start = captured.len();
                for (capture, ty) in captures {
                    captured.push(Parameter {
                        category: Category::of_descriptor(&type_descriptor(ty)),
                        binding: self.capture_binding(capture, ty),
                    });
                }
                lambda.captured = start..captured.len();
                lambda_bindings.insert(index, lambdas.len());
                lambdas.push(lambda);
            }
            let loaded = self
                .load_default_lambdas(call, &default_lambdas, &physical)
                .and_then(|loaded| {
                    for (operand, mut lambda) in loaded {
                        default_bridge::append_default_lambda_captures(&mut lambda, &mut captured)?;
                        lambda_bindings.insert(operand, lambdas.len());
                        lambdas.push(lambda);
                    }
                    Ok(())
                });
            loaded.map_err(|error| {
                crate::trace_compiler!("splice", "a default lambda is not inlined: {error:?}");
                "a default lambda of the call cannot be inlined"
            })?;
            let parameters = self.call_parameters(
                &supplies,
                &physical,
                args,
                &lambda_bindings,
                captured,
                &lambdas,
            );
            // kotlinc's `MethodInliner` regenerates every object it meets; only a body krusty wrote
            // can lack the `$$forInline` copy a regenerated suspend method is built over.
            if inliner::suspend_lambda_without_for_inline_copy(
                &callee,
                &parameters,
                &lambdas,
                &self.bodies,
            )
            .map_err(|error| {
                crate::trace_compiler!("splice", "the call's objects do not read: {error:?}");
                "an anonymous object a compiled inline body constructs cannot be read"
            })? {
                return splice(SpliceReason::SuspendMethodWithoutForInlineCopy);
            }
        }
        Ok(LambdaCallRoute::MethodInliner(callee, default_lambdas))
    }

    /// Whether `callee`, the body of `inline_call`, never loads a parameter that takes one of the
    /// call's inlined literals. kotlinc drops such a literal with the parameter: nothing of it
    /// reaches the caller.
    pub(super) fn inlined_literals_unread(
        &self,
        inline_call: &ClasspathInlineCall<'_, '_>,
        callee: &MethodNode,
    ) -> bool {
        let ClasspathInlineCall {
            call_expression,
            target,
            args,
            leading_non_argument_operands,
            ..
        } = *inline_call;
        let Some(physical) = parse_descriptor_params(target.splice_desc) else {
            return false;
        };
        let Ok(positions) =
            self.inlined_literal_positions(call_expression, leading_non_argument_operands, args)
        else {
            return false;
        };
        positions.iter().all(|&position| {
            let slot = physical
                .iter()
                .take(position)
                .map(|&ty| slot_words(ty))
                .sum();
            !inliner::reads_local(callee, slot)
        })
    }
}
