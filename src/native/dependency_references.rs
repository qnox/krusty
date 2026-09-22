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

/// The accessors synthesized for one reference to a DEPENDENCY property, and what the reference
/// object needs beside them.
///
/// The object itself is the same one a reference to a property of this file becomes
/// ([`codegen::lower::references`]): one emitted type per property, `get`, `set` and `name` in its
/// table, and the bound receiver in its one field. What it needs that the checked node does not
/// carry is a pair of FUNCTIONS to reach the property through — a dependency property has no
/// storage here and no accessor of this file's to name — so the pass makes them.
pub(crate) struct DependencyProperty {
    /// The getter's provider identity, which is where the property's Kotlin NAME is read from: the
    /// reference site's spelling is a different fact (a lookup may reach it under an alias) and
    /// the accessor's own name is a physical call target.
    pub(crate) property: crate::fir::ExternalPropertyId,
    pub(crate) getter: crate::ir::FunId,
    pub(crate) setter: Option<crate::ir::FunId>,
    /// Whether the accessors lead with a receiver, which is what `get` hands over for an unbound
    /// site and what the object's field holds for a bound one.
    pub(crate) receiver: bool,
    pub(crate) ty: Ty,
    pub(crate) mutable: bool,
    /// The expression supplying `x` in `x::p`.
    pub(crate) bound: Option<ExprId>,
}

/// Synthesize the accessors for every realizable dependency property reference in `ir`.
///
/// The checked node is LEFT as it is: unlike a callable reference, which becomes a function value
/// the generator already lowers, a property reference has no common-IR shape to become — the
/// object is the generator's own. So this only supplies what the object is built from, keyed by the
/// site, and a site this cannot supply for is absent and declines by name as before.
pub(crate) fn realize_properties(
    ir: &mut IrFile,
) -> std::collections::HashMap<ExprId, DependencyProperty> {
    let mut realized = std::collections::HashMap::new();
    for index in 0..ir.exprs.len() {
        let Ok(plan) = property_plan(ir, index as ExprId) else {
            continue;
        };
        realized.insert(index as ExprId, build_property(ir, plan));
    }
    realized
}

/// Why a reference to a DEPENDENCY property was not supplied accessors, named rather than
/// numbered; `None` when the node is no such reference.
///
/// The same conditions [`property_plan`] decides by, read again to report them — so the decline
/// says which shape it was rather than that the file did not declare the property, which every one
/// of them has in common.
pub(crate) fn property_decline(ir: &IrFile, id: ExprId) -> String {
    match property_plan(ir, id) {
        // The accessors were made and the declare pass records its own reason for every site it
        // then leaves out, so this arm is a fallback rather than a reason anything reaches; see
        // `codegen::lower::references::declare_dependency_property_references`.
        Ok(_) => "a reference to a dependency property this generator left undeclared".to_string(),
        Err(None) => "a property reference of a shape this pass does not read".to_string(),
        Err(Some(reason)) => reason,
    }
}

/// What a checked property-reference node yields, read before anything is synthesized.
struct PropertyPlan {
    property: crate::fir::ExternalPropertyId,
    getter: AccessorPlan,
    setter: Option<AccessorPlan>,
    receiver: Option<Ty>,
    ty: Ty,
    mutable: bool,
    bound: Option<ExprId>,
}

/// One accessor of a dependency property, as its target states it.
struct AccessorPlan {
    property: crate::fir::ExternalPropertyId,
    dispatch: crate::ir::IrPropertyDispatch,
    receiver: Option<Ty>,
    parameters: Vec<Ty>,
    result: Ty,
}

/// Read one accessor's target, or say why it is not one this can reach.
fn accessor_plan(
    target: &crate::fir::FirPropertyTarget,
    written: usize,
    which: &str,
) -> Result<AccessorPlan, String> {
    let crate::fir::FirPropertyTarget::External {
        property,
        receiver,
        parameters,
        result,
        extension_receiver_parameter,
        dispatch,
    } = target
    else {
        return Err(format!(
            "a dependency property whose {which} is no dependency accessor"
        ));
    };
    // A MEMBER EXTENSION property's accessor wants two receivers, and the object has room for one.
    if extension_receiver_parameter.is_some() {
        return Err(format!(
            "a reference to a member extension property of a dependency, whose {which} wants two \
             receivers where this object has room for one"
        ));
    }
    // The getter takes nothing beyond its receiver and the setter takes the written value; an
    // accessor wanting more has operands this object does not carry — a property with context
    // parameters is that shape.
    if parameters.len() != written {
        return Err(format!(
            "a reference to a dependency property whose {which} takes {} operands where this \
             object carries {written}",
            parameters.len()
        ));
    }
    Ok(AccessorPlan {
        property: *property,
        dispatch: match dispatch {
            crate::fir::FirPropertyDispatch::Ordinary => crate::ir::IrPropertyDispatch::Ordinary,
            crate::fir::FirPropertyDispatch::Super { owner, interface } => {
                crate::ir::IrPropertyDispatch::Super {
                    owner: *owner,
                    interface: *interface,
                }
            }
        },
        receiver: receiver.map(|receiver| receiver.get()),
        parameters: parameters.iter().map(|parameter| parameter.get()).collect(),
        result: result.get(),
    })
}

/// The plan a checked property-reference node yields.
///
/// `Err(None)` where the node is no reference to a dependency property at all — every other node
/// in the arena reaches here — and `Err(Some(reason))` where it is one and the shape is not
/// realizable, so the decline can name it.
fn property_plan(ir: &IrFile, id: ExprId) -> Result<PropertyPlan, Option<String>> {
    let IrExpr::Checked(IrCheckedOperation::PropertyReference {
        target:
            crate::fir::FirPropertyReferenceTarget::External {
                getter,
                setter,
                property_type,
                ..
            },
        delegated: _,
        binding: _,
        dispatch_receiver,
        extension_receiver,
        mutable,
        substitutions: _,
        adaptation,
    }) = ir.expr(id)
    else {
        return Err(None);
    };
    let reason = |text: String| Err(Some(text));
    if adaptation.is_some() {
        return reason("an ADAPTED reference to a dependency property".to_string());
    }
    // The object carries at most ONE receiver, and which of the two it is does not matter to it;
    // BOTH at once is a member extension, whose accessor wants two.
    let bound = match (dispatch_receiver, extension_receiver) {
        (None, None) => None,
        (Some(receiver), None) | (None, Some(receiver)) => Some(*receiver),
        (Some(_), Some(_)) => {
            return reason(
                "a reference to a member extension property of a dependency, which binds two \
                 receivers where this object has room for one"
                    .to_string(),
            )
        }
    };
    let getter = accessor_plan(getter, 0, "getter").map_err(Some)?;
    let setter = match setter {
        Some(setter) => Some(accessor_plan(setter, 1, "setter").map_err(Some)?),
        None => None,
    };
    // Both accessors reach the same object, so they lead with the same receiver or with none.
    if setter
        .as_ref()
        .is_some_and(|setter| setter.receiver != getter.receiver)
    {
        return reason(
            "a reference to a dependency property whose accessors lead with different receivers"
                .to_string(),
        );
    }
    // A bound site has a receiver to hold, an unbound one is handed it at `get`; either way the
    // accessor has to take one, or take none and be given none.
    if getter.receiver.is_none() && bound.is_some() {
        return reason(
            "a reference binding a receiver to a dependency property whose getter takes none"
                .to_string(),
        );
    }
    Ok(PropertyPlan {
        property: getter.property,
        receiver: getter.receiver,
        getter,
        setter,
        ty: property_type.get(),
        mutable: *mutable,
        bound,
    })
}

/// Emit the accessor pair and record what the reference object is built from.
fn build_property(ir: &mut IrFile, plan: PropertyPlan) -> DependencyProperty {
    let PropertyPlan {
        property,
        getter,
        setter,
        receiver,
        ty,
        mutable,
        bound,
    } = plan;
    let leading = usize::from(receiver.is_some());
    let read = {
        let operand = receiver.map(|_| ir.add_expr(IrExpr::GetValue(0)));
        let value = ir.add_expr(IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
            target: getter.property,
            dispatch: getter.dispatch,
            receiver: operand,
            arguments: Vec::new(),
            parameters: getter.parameters,
            result: getter.result,
            source_receiver: None,
        }));
        let returned = ir.add_expr(IrExpr::Return(Some(value)));
        ir.add_expr(IrExpr::Block {
            stmts: vec![returned],
            value: None,
        })
    };
    let read = ir.add_fun(IrFunction {
        name: format!("$native_dependency_get_{}", ir.functions.len()),
        params: receiver.into_iter().collect(),
        ret: getter.result,
        body: Some(read),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    ir.private_methods.insert(read);
    let written = setter.map(|setter| {
        let operand = receiver.map(|_| ir.add_expr(IrExpr::GetValue(0)));
        let value = ir.add_expr(IrExpr::GetValue(leading as u32));
        let write = ir.add_expr(IrExpr::Checked(IrCheckedOperation::ExternalPropertyWrite {
            target: setter.property,
            dispatch: setter.dispatch,
            receiver: operand,
            arguments: vec![value],
            parameters: setter.parameters.clone(),
            result: setter.result,
            source_receiver: None,
        }));
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![write],
            value: None,
        });
        let mut params: Vec<Ty> = receiver.into_iter().collect();
        params.extend(setter.parameters.iter().copied());
        let function = ir.add_fun(IrFunction {
            name: format!("$native_dependency_set_{}", ir.functions.len()),
            params,
            ret: Ty::Unit,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        ir.private_methods.insert(function);
        function
    });
    DependencyProperty {
        property,
        getter: read,
        setter: written,
        receiver: receiver.is_some(),
        ty,
        mutable,
        bound,
    }
}
