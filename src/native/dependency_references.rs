//! `"KOTLIN"::get`, `Boolean::not`, `String::plus` — a callable reference to a DEPENDENCY
//! declaration, realized as the adapter it already is everywhere else.
//!
//! Common lowering builds such a reference into an ordinary function value whenever the site's
//! type is a plain `FunctionN` ([`fir_lower::external_references`]): a synthesized static method
//! that calls the dependency, plus a value that carries whatever receiver was bound. What it
//! deliberately does NOT do is build one when the site's type is a REFLECTIVE `KFunctionN`, because
//! a reflection value is a platform representation and the choice of one belongs to a target.
//!
//! This backend's choice is the same object, with the declaration's identity kept beside it — the
//! reference answers `equals`, `hashCode` and `name` off that identity exactly as a reference to a
//! declaration of this file does ([`codegen::lower::functions`],
//! [`codegen::lower::references::callable_reference_name`]). Nothing here is Kotlin semantics being
//! decided a second time: the checked node already names the declaration, says which receiver is
//! bound, and carries the parameter and result types the adapter takes. Making the adapter is
//! mechanical, and doing it on the IR rather than in the generator is what lets the reference reach
//! the dependency through the ordinary dependency-call path, with every intrinsic, value class and
//! runtime entry point that path knows.
//!
//! A site this cannot build is LEFT as it is, so the generator declines it by name rather than
//! emitting an adapter that calls the wrong thing.

use crate::fir::{FirCallableReferenceBinding, FirCallableReferenceTarget};
use crate::ir::{
    Callee, ExprId, IrCallableReference, IrCallableReferenceTarget, IrCheckedOperation, IrExpr,
    IrFile, IrFunction, IrTypeOp,
};
use crate::types::Ty;

/// Rewrite every realizable dependency callable reference in `ir` into the adapter form.
pub(crate) fn realize(ir: &mut IrFile) {
    for index in 0..ir.exprs.len() {
        let Some(plan) = plan(ir, index as ExprId) else {
            continue;
        };
        let reference = build(ir, plan);
        ir.exprs[index] = IrExpr::CallableReference(reference);
    }
}

/// Everything the rewrite needs, read out of the checked node.
struct Plan {
    declaration: crate::fir::ExternalCallableId,
    default_provider: Option<crate::fir::ExternalCallableId>,
    /// The type of the receiver the declaration takes, when it takes one.
    receiver: Option<Ty>,
    parameters: Vec<Ty>,
    result: Ty,
    substitutions: Vec<crate::ir::IrCheckedSubstitution>,
    /// The expression supplying `x` in `x::f`, for a bound site.
    bound: Option<ExprId>,
    /// The types the reference VALUE takes and answers, which the adapter's own parameters are.
    reference_params: Vec<Ty>,
    reference_ret: Ty,
    /// The site's own type, which is what a use of the value is checked against.
    function_type: Ty,
}

/// The plan a checked node yields, or `None` when this pass leaves the node alone.
fn plan(ir: &IrFile, id: ExprId) -> Option<Plan> {
    let IrExpr::Checked(IrCheckedOperation::CallableReference {
        target:
            FirCallableReferenceTarget::External {
                declaration,
                default_provider,
                receiver,
                extension_receiver: _,
                parameters,
                result,
            },
        binding,
        dispatch_receiver,
        extension_receiver,
        function_type,
        substitutions,
        adaptation,
    }) = ir.expr(id)
    else {
        return None;
    };
    // An ADAPTED reference reorders, defaults or vararg-packs its operands on the way through, and
    // the adapter would have to do that packing. Common lowering can, because it owns argument
    // normalization; this pass builds a call with the operands in declaration order and nothing
    // else, so an adaptation is left to decline.
    if adaptation.is_some() {
        return None;
    }
    let Ty::Fun(reference) = function_type.non_null() else {
        return None;
    };
    // A MEMBER EXTENSION wants two receivers and the target carries one type for them, so which is
    // which could only be guessed at.
    if dispatch_receiver.is_some() && extension_receiver.is_some() {
        return None;
    }
    let bound = dispatch_receiver.or(*extension_receiver);
    // A bound site has its receiver in hand; an unbound one takes it as the first parameter the
    // caller supplies, so a declaration that has a receiver needs one there to take.
    match (binding, receiver, bound) {
        (FirCallableReferenceBinding::Unbound, Some(_), None) => {
            if reference.params.is_empty() {
                return None;
            }
        }
        (FirCallableReferenceBinding::Unbound, None, None) => {}
        (FirCallableReferenceBinding::Bound, Some(_), Some(_)) => {}
        _ => return None,
    }
    let own = usize::from(receiver.is_some() && bound.is_none());
    if reference.params.len() - own != parameters.len() {
        return None;
    }
    Some(Plan {
        declaration: *declaration,
        default_provider: *default_provider,
        receiver: receiver.map(|receiver| receiver.get()),
        parameters: parameters.iter().map(|parameter| parameter.get()).collect(),
        result: result.get(),
        substitutions: substitutions.clone(),
        bound,
        reference_params: reference.params.to_vec(),
        reference_ret: reference.ret,
        function_type: *function_type,
    })
}

/// Synthesize the adapter and the reference value that names it.
///
/// The adapter's value slots are the captures first and its own parameters after, which is the
/// frame [`codegen::lower::functions::closure_site`] reads a reference's captures in. A bound site
/// captures the receiver; an unbound one captures nothing and reads the receiver out of parameter
/// zero.
fn build(ir: &mut IrFile, plan: Plan) -> IrCallableReference {
    let Plan {
        declaration,
        default_provider,
        receiver,
        parameters,
        result,
        substitutions,
        bound,
        reference_params,
        reference_ret,
        function_type,
    } = plan;
    let own_start = u32::from(bound.is_some());
    let mut slots = (0..reference_params.len() as u32).map(|slot| own_start + slot);
    let dispatch_receiver = receiver.map(|_| match bound {
        Some(_) => ir.add_expr(IrExpr::GetValue(0)),
        None => {
            let slot = slots.next().expect("an unbound receiver has a parameter");
            ir.add_expr(IrExpr::GetValue(slot))
        }
    });
    let args = slots
        .map(|slot| ir.add_expr(IrExpr::GetValue(slot)))
        .collect::<Vec<_>>();
    let call = ir.add_expr(IrExpr::Call {
        callee: Callee::External {
            target: declaration,
            default_provider,
            params: parameters,
            ret: result,
            substitutions,
            defaults: Vec::new(),
            extension_receiver_parameter: None,
        },
        dispatch_receiver,
        args,
    });
    // What the reference VALUE answers, which is not always what the declaration returns: a
    // `KFunction1<Int, Any>` over `String.get` answers `Any`, and `Unit` is answered by the
    // singleton rather than by the call.
    let value = if reference_ret == Ty::Unit {
        let unit = ir.add_expr(IrExpr::UnitInstance);
        ir.add_expr(IrExpr::Block {
            stmts: vec![call],
            value: Some(unit),
        })
    } else if result == reference_ret {
        call
    } else {
        ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: call,
            type_operand: reference_ret,
        })
    };
    let returned = ir.add_expr(IrExpr::Return(Some(value)));
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![returned],
        value: None,
    });
    let mut params = Vec::with_capacity(reference_params.len() + own_start as usize);
    if bound.is_some() {
        params.push(receiver.expect("a bound reference has a receiver type"));
    }
    params.extend(reference_params.iter().copied());
    let adapter = ir.add_fun(IrFunction {
        name: format!("$native_dependency_ref_{}", ir.functions.len()),
        params,
        ret: crate::types::stored_value_ty(reference_ret),
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    ir.private_methods.insert(adapter);
    ir.lambda_own_params_from.insert(adapter, own_start);
    IrCallableReference {
        target: IrCallableReferenceTarget::External { declaration },
        adapter,
        captures: Vec::new(),
        bound_receiver: bound,
        function_type,
        declaration_parameters: reference_params.into_boxed_slice(),
        declaration_result: result,
        declaration_suspend: false,
        adaptation: None,
    }
}
