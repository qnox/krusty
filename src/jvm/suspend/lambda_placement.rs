//! Where a classpath `suspend inline` callee places the suspend lambdas a call passes it.
//!
//! The method inliner places an inline lambda wherever the callee's compiled body uses it: an
//! `invoke` of it becomes its body in the caller's frame, and a lambda the body hands to an
//! anonymous object's constructor runs in the regenerated object's method instead. Coroutine
//! lowering has to know which before it builds the caller's machine, since only a lambda placed in
//! the frame contributes suspension points to it. The callee's declaration says neither, so the
//! answer is read from the body the emitter splices, before lowering starts.

use crate::fx_hash::HashMap;
use crate::ir::{Callee, ExprId, IrExpr, IrFile};
use crate::jvm::classpath::Classpath;
use crate::jvm::default_call_operands::DefaultCallOperands;
use crate::jvm::inline::MethodBodies;
use crate::jvm::inliner::{lambda_placements, Binding, LambdaPlacement, Parameter, Parameters};
use crate::jvm::method_node::{Category, MethodNode};
use crate::libraries::InlineKind;

const ACC_STATIC: u16 = 0x0008;

/// Record in [`IrFile::retained_suspend_lambdas`] the argument positions of every call to a
/// classpath `suspend inline` function whose suspend lambda the callee's body passes to an object
/// it constructs. Every call that passes a suspend lambda to an inline parameter of such a callee
/// is listed, with no position when the body invokes each lambda itself. `Err` explains a call
/// whose body is unavailable or places a lambda some other way: its suspensions have no frame.
pub(crate) fn record(
    ir: &mut IrFile,
    classpath: &Classpath,
    default_call_operands: &DefaultCallOperands,
) -> Result<(), String> {
    let mut retained = HashMap::default();
    for call in 0..ir.exprs.len() {
        let call = ExprId::try_from(call).expect("IR expression index exceeds ExprId");
        let IrExpr::Call {
            callee,
            dispatch_receiver,
            args,
        } = &ir.exprs[call as usize]
        else {
            continue;
        };
        let Callee::Static {
            owner,
            name,
            descriptor,
            inline: InlineKind::CanInline,
        } = &**callee
        else {
            continue;
        };
        if !ir.suspend_calls.contains_key(&call) || ir.reified_call_subst.contains_key(&call) {
            continue;
        }
        let lambdas: Vec<usize> = (0..args.len())
            .filter(|&position| {
                crate::jvm::inline_parameters::is_inline_parameter(ir, call, position) == Some(true)
                    && matches!(&ir.exprs[args[position] as usize], IrExpr::Lambda { impl_fn, .. }
                        if ir.suspend_funs.contains(impl_fn))
            })
            .collect();
        if lambdas.is_empty() {
            continue;
        }
        // The body the emitter splices: the `$$forInline` copy of a callable function, or the
        // `$default` bridge, whose descriptor already holds the continuation.
        let owner = owner.render();
        let (body_name, physical) = if default_call_operands.contains(call) {
            (name.clone(), descriptor.clone())
        } else {
            (
                format!("{name}$$forInline"),
                super::call_operand_realization::cps_descriptor(descriptor),
            )
        };
        let unavailable = |reason: String| {
            format!("where {owner}.{body_name}{physical} places its suspend lambdas: {reason}")
        };
        let body = classpath
            .method_code(&owner, &body_name, &physical)
            .ok_or_else(|| unavailable("the body is not on the classpath".to_string()))?;
        let access = if dispatch_receiver.is_some() {
            0
        } else {
            ACC_STATIC
        };
        let node = MethodNode::read(access, &body_name, &physical, &body)
            .map_err(|error| unavailable(format!("{error:?}")))?;
        let parameters = bound_parameters(&physical, dispatch_receiver.is_some(), &lambdas)
            .ok_or_else(|| unavailable("a malformed descriptor".to_string()))?;
        let bodies: &dyn MethodBodies = classpath;
        let placements =
            lambda_placements(&node, &parameters, lambdas.len(), &bodies).map_err(unavailable)?;
        let positions = lambdas
            .iter()
            .zip(placements)
            .filter(|&(_, placement)| placement == LambdaPlacement::Retained)
            .map(|(&position, _)| u32::try_from(position).expect("argument position exceeds u32"))
            .collect();
        retained.insert(call, positions);
    }
    ir.retained_suspend_lambdas = retained;
    Ok(())
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

/// Whether the callee of `call` hands the lambda at argument `position` to an object it constructs,
/// as [`record`] read from its body.
pub(crate) fn retained_in_an_object(ir: &IrFile, call: ExprId, position: usize) -> bool {
    ir.retained_suspend_lambdas
        .get(&call)
        .is_some_and(|positions| {
            positions
                .iter()
                .any(|&retained| retained as usize == position)
        })
}
