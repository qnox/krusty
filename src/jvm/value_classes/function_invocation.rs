//! A direct call of a value class that implements a function type.
//!
//! `ValueClass(1)(1)` is a function-value invocation in common IR, so it would otherwise call
//! `FunctionN.invoke` on the carrier. kotlinc keeps the carrier and calls the value class's static
//! implementation (`invoke-impl` or the signature hash) with that carrier as argument zero.

use std::collections::HashMap;

use super::member_names::vc_member_impl_name;
use super::representation::erase;
use super::{ir_method_desc, ReprCtx, Under};
use crate::ir::{Callee, ExprId, IrClass, IrExpr, IrFile, IrFunction};
use crate::libraries::InlineKind;
use crate::types::{Ty, TypeName};

pub(super) struct Plan {
    owner: TypeName,
    name: String,
    parameters: Vec<Ty>,
    result: Ty,
    receiver: ExprId,
    arguments: Vec<ExprId>,
    semantic_parameters: Vec<Ty>,
}

pub(super) struct Lookup<'a> {
    pub functions: &'a [IrFunction],
    pub classes: &'a [IrClass],
    pub class_index: &'a HashMap<TypeName, usize>,
    pub logical_types: &'a HashMap<ExprId, Ty>,
    pub suspend_calls: &'a HashMap<ExprId, Ty>,
    pub module_value_classes: &'a HashMap<TypeName, Ty>,
    pub under: &'a Under,
    pub callable_under: &'a Under,
    pub repr_ctx: &'a ReprCtx<'a>,
}

pub(super) fn plan(lookup: &Lookup<'_>, id: ExprId, expression: &IrExpr) -> Option<Plan> {
    let Lookup {
        functions,
        classes,
        class_index,
        logical_types,
        suspend_calls,
        module_value_classes,
        under,
        callable_under,
        repr_ctx,
    } = lookup;
    let IrExpr::InvokeFunction {
        func,
        args: arguments,
        params: parameters,
        ret: node_result,
    } = expression
    else {
        return None;
    };
    if suspend_calls.contains_key(&id) {
        return None;
    }
    let owner = callee_value_class(logical_types, repr_ctx, under, *func)?;
    let result = logical_types.get(&id).copied().unwrap_or(*node_result);
    let name = vc_member_impl_name("invoke", parameters, &result, callable_under, false);
    let (name, physical_parameters, physical_result) = if let Some(&index) = class_index.get(&owner)
    {
        let (parameters, result) = classes[index].methods.iter().copied().find_map(|fid| {
            let function = functions.get(fid as usize)?;
            (function.is_static
                && function.name == name
                && function.params.len() == parameters.len() + 1)
                .then(|| (function.params.clone(), function.ret))
        })?;
        (name, parameters, result)
    } else if module_value_classes.contains_key(&owner) {
        let carrier = erase(under.get(&owner)?, under);
        let physical_parameters = std::iter::once(carrier)
            .chain(
                parameters
                    .iter()
                    .copied()
                    .map(|parameter| erase(&parameter, under)),
            )
            .collect();
        (name, physical_parameters, erase(&result, under))
    } else {
        return None;
    };
    let mut semantic_parameters = Vec::with_capacity(parameters.len() + 1);
    semantic_parameters.push(Ty::obj_name(owner));
    semantic_parameters.extend(parameters.iter().copied());
    Some(Plan {
        owner,
        name,
        parameters: physical_parameters,
        result: physical_result,
        receiver: *func,
        arguments: arguments.to_vec(),
        semantic_parameters,
    })
}

pub(super) fn apply(ir: &mut IrFile, id: ExprId, plan: Plan) -> IrExpr {
    ir.physical_types.insert(id, plan.result);
    ir.call_declared_params
        .insert(id, plan.semantic_parameters.into_boxed_slice());
    let mut arguments = Vec::with_capacity(plan.arguments.len() + 1);
    arguments.push(plan.receiver);
    arguments.extend(plan.arguments);
    IrExpr::Call {
        callee: Callee::Static {
            owner: plan.owner,
            name: plan.name,
            descriptor: ir_method_desc(&plan.parameters, &plan.result),
            inline: InlineKind::None,
        },
        dispatch_receiver: None,
        args: arguments,
    }
}

fn callee_value_class(
    logical_types: &HashMap<ExprId, Ty>,
    repr_ctx: &ReprCtx<'_>,
    under: &Under,
    func: ExprId,
) -> Option<TypeName> {
    if let Some(ty) = logical_types.get(&func).copied() {
        if ty.is_nullable() {
            return None;
        }
        let owner = ty.non_null().obj_internal()?;
        return under.contains_key(&owner).then_some(owner);
    }
    match repr_ctx.repr(func) {
        super::Repr::Unboxed(owner) => Some(owner),
        super::Repr::Boxed(_) | super::Repr::NotVc => None,
    }
}
