//! Where a classpath `suspend inline` callee places the suspend lambdas a call passes it.
//!
//! The method inliner places an inline lambda wherever the callee's compiled body uses it: an
//! `invoke` of it becomes its body in the caller's frame, and a lambda the body hands to an
//! anonymous object's constructor runs in the regenerated object's method instead. Coroutine
//! lowering has to know which before it builds the caller's machine, since only a lambda placed in
//! the frame contributes suspension points to it. The callee's declaration says neither, so the
//! answer is read from the body the emitter splices, before lowering starts.

use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{for_each_child, Callee, ExprId, InlineLambdaPlacement, IrExpr, IrFile};
use crate::jvm::classpath::Classpath;
use crate::jvm::default_call_operands::DefaultCallOperands;
use crate::jvm::inline::MethodBodies;
use crate::jvm::inliner::{lambda_placements, Binding, Parameter, Parameters};
use crate::jvm::method_node::{Category, MethodNode};
use crate::libraries::InlineKind;

const ACC_STATIC: u16 = 0x0008;

/// Record in [`IrFile::suspend_lambda_placements`] where each suspend literal an emitted inline call
/// binds to an inline parameter runs. A call that is no suspension cannot invoke a suspend lambda,
/// only hand it to an object it constructs. A classpath `suspend inline` callee spliced from its
/// compiled body places it wherever that body uses it. Any other suspending inline call places it
/// in the caller's frame, as its own expansion runs there.
///
/// Only calls some emitted root reaches are read: earlier passes leave the nodes of a rewritten
/// body behind in the arena, and nothing is spliced for those. `Err` explains a call whose body is
/// unavailable or places a lambda some other way: its suspensions would have no frame.
pub(crate) fn record(
    ir: &mut IrFile,
    classpath: &Classpath,
    default_call_operands: &DefaultCallOperands,
) -> Result<(), String> {
    let suspend_set: HashSet<u32> = ir.suspend_funs.iter().copied().collect();
    let mut recorded = HashMap::default();
    for call in emitted_calls(ir) {
        let IrExpr::Call { args, .. } = &ir.exprs[call as usize] else {
            continue;
        };
        let lambdas: Vec<usize> = (0..args.len())
            .filter(|&position| {
                crate::jvm::inline_parameters::is_inline_parameter(ir, call, position) == Some(true)
                    && matches!(&ir.exprs[args[position] as usize], IrExpr::Lambda { impl_fn, .. }
                        if suspend_set.contains(impl_fn))
            })
            .collect();
        if lambdas.is_empty() {
            continue;
        }
        let placements = if !super::is_suspension_point(ir, call, &suspend_set) {
            vec![InlineLambdaPlacement::Retained; lambdas.len()]
        } else if let Some(body) = spliced_body(ir, call, default_call_operands) {
            body.placements(classpath, &lambdas)?
        } else {
            vec![InlineLambdaPlacement::Invoked; lambdas.len()]
        };
        for (position, placement) in lambdas.into_iter().zip(placements) {
            recorded.insert(args[position], placement);
        }
    }
    ir.suspend_lambda_placements = recorded;
    Ok(())
}

/// The compiled body a call to a classpath `suspend inline` function splices.
struct SplicedBody {
    owner: String,
    name: String,
    descriptor: String,
    receiver: bool,
}

/// The body the emitter splices for `call`, a suspension: the `$$forInline` copy of a callable
/// classpath inline function, or its `$default` bridge, whose descriptor already holds the
/// continuation. `None` for any other callee.
fn spliced_body(
    ir: &IrFile,
    call: ExprId,
    default_call_operands: &DefaultCallOperands,
) -> Option<SplicedBody> {
    let IrExpr::Call {
        callee,
        dispatch_receiver,
        ..
    } = &ir.exprs[call as usize]
    else {
        return None;
    };
    let Callee::Static {
        owner,
        name,
        descriptor,
        inline: InlineKind::CanInline,
    } = &**callee
    else {
        return None;
    };
    if !ir.suspend_calls.contains_key(&call) || ir.reified_call_subst.contains_key(&call) {
        return None;
    }
    let (name, descriptor) = if default_call_operands.contains(call) {
        (name.clone(), descriptor.clone())
    } else {
        (
            format!("{name}$$forInline"),
            super::call_operand_realization::cps_descriptor(descriptor),
        )
    };
    Some(SplicedBody {
        owner: owner.render(),
        name,
        descriptor,
        receiver: dispatch_receiver.is_some(),
    })
}

impl SplicedBody {
    /// Where the body places the call's arguments at `lambdas`.
    fn placements(
        &self,
        classpath: &Classpath,
        lambdas: &[usize],
    ) -> Result<Vec<InlineLambdaPlacement>, String> {
        let Self {
            owner,
            name,
            descriptor,
            receiver,
        } = self;
        let unavailable = |reason: String| {
            format!("where {owner}.{name}{descriptor} places its suspend lambdas: {reason}")
        };
        let body = classpath
            .method_code(owner, name, descriptor)
            .ok_or_else(|| unavailable("the body is not on the classpath".to_string()))?;
        let access = if *receiver { 0 } else { ACC_STATIC };
        let node = MethodNode::read(access, name, descriptor, &body)
            .map_err(|error| unavailable(format!("{error:?}")))?;
        let parameters = bound_parameters(descriptor, *receiver, lambdas)
            .ok_or_else(|| unavailable("a malformed descriptor".to_string()))?;
        let bodies: &dyn MethodBodies = classpath;
        lambda_placements(&node, &parameters, lambdas.len(), &bodies).map_err(unavailable)
    }
}

/// Every call an emitted root reaches, each once, in arena order. A lambda's spliced body is
/// reached through the lambda, as the emitter reaches it.
fn emitted_calls(ir: &IrFile) -> Vec<ExprId> {
    let mut seen = HashSet::default();
    let mut pending = crate::jvm::lambda_classes::emitted_roots(ir, true);
    let mut calls = Vec::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if matches!(ir.exprs[expression as usize], IrExpr::Call { .. }) {
            calls.push(expression);
        }
        for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    calls.sort_unstable();
    calls
}

/// The callee's parameters, `this` first when it has one, with the argument at each of `lambdas`
/// bound to its inline lambda.
fn bound_parameters(descriptor: &str, receiver: bool, lambdas: &[usize]) -> Option<Parameters> {
    let arguments = crate::jvm::bytecode_passes::descriptors::argument_types(descriptor)?;
    let receiver = receiver.then_some(Parameter {
        category: Category::Reference,
        binding: Binding::Temporary,
    });
    let parameters = receiver
        .into_iter()
        .chain(
            arguments
                .iter()
                .enumerate()
                .map(|(position, argument)| Parameter {
                    category: Category::of_descriptor(argument),
                    binding: match lambdas.iter().position(|&lambda| lambda == position) {
                        Some(lambda) => Binding::Lambda(lambda),
                        None => Binding::Temporary,
                    },
                }),
        )
        .collect();
    Some(Parameters {
        parameters,
        captured: Vec::new(),
        capture_owners: Vec::new(),
    })
}

/// Where the body of the suspend literal `lambda` runs, as [`record`] decided from the emitted
/// inline call that binds it; `None` for a value no emitted inline call binds.
pub(crate) fn recorded_placement(ir: &IrFile, lambda: ExprId) -> Option<InlineLambdaPlacement> {
    ir.suspend_lambda_placements.get(&lambda).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrConst, IrFunction};
    use crate::types::{InlineParameterModifier, Ty};

    /// A file whose function `root` evaluates `Const`, with a call to an inline suspend callee the
    /// classpath does not have, passing a suspend lambda. `reached` makes the call `root`'s body.
    fn file_with_call(reached: bool) -> (IrFile, ExprId) {
        let mut ir = IrFile::default();
        let lambda_fn: u32 = 1;
        ir.suspend_funs.push(lambda_fn);
        let lambda = ir.add_expr(IrExpr::Lambda {
            impl_fn: lambda_fn,
            arity: 1,
            captures: Vec::new(),
            sam: None,
            inline_body: None,
        });
        let call = ir.add_expr(IrExpr::Call {
            callee: Box::new(Callee::Static {
                owner: crate::types::type_name("lib/MissingKt"),
                name: "absent".to_string(),
                descriptor: "(Lkotlin/jvm/functions/Function1;)Ljava/lang/Object;".to_string(),
                inline: InlineKind::CanInline,
            }),
            dispatch_receiver: None,
            args: vec![lambda],
        });
        ir.suspend_calls.insert(call, Ty::Int);
        ir.call_declared_params
            .insert(call, vec![Ty::fun(Vec::new(), Ty::Int)].into_boxed_slice());
        ir.call_inline_modifiers.insert(
            call,
            vec![InlineParameterModifier::Crossinline].into_boxed_slice(),
        );
        let body = if reached {
            call
        } else {
            ir.add_expr(IrExpr::Const(IrConst::Int(0)))
        };
        for (name, body) in [("root", Some(body)), ("lambda", None)] {
            ir.functions.push(IrFunction {
                name: name.to_owned(),
                params: Vec::new(),
                ret: Ty::Int,
                body,
                is_static: true,
                dispatch_receiver: None,
                param_checks: Vec::new(),
            });
        }
        (ir, call)
    }

    #[test]
    fn a_stale_call_no_root_reaches_has_no_body_to_read() {
        let (mut ir, _) = file_with_call(false);
        record(
            &mut ir,
            &Classpath::empty(),
            &DefaultCallOperands::default(),
        )
        .expect("no emitted call needs a body");
        assert!(ir.suspend_lambda_placements.is_empty());
    }

    #[test]
    fn an_emitted_call_without_its_body_is_refused() {
        let (mut ir, _) = file_with_call(true);
        assert_eq!(
            record(
                &mut ir,
                &Classpath::empty(),
                &DefaultCallOperands::default()
            ),
            Err("where lib/MissingKt.absent$$forInline\
                 (Lkotlin/jvm/functions/Function1;Lkotlin/coroutines/Continuation;)\
                 Ljava/lang/Object; places its suspend lambdas: the body is not on the classpath"
                .to_string())
        );
    }
}
