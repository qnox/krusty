//! Physical names and descriptors of calls to a private property's synthetic accessor.
//!
//! Module realization records each such call with the source arguments' declared parameters in
//! `call_declared_params`. A member's accessor is a static of its class that takes the owner, the
//! call's dispatch receiver, first, then those parameters over their value-class carriers; this
//! pass writes that descriptor. A top-level property's accessor is a facade call whose parameters
//! the cross-file pass erases. Either accessor that calls a declared accessor is named after that
//! accessor's own JVM name, which kotlinc mangles for a value-class signature
//! (`access$getWeight-b--Cat4`). A field accessor keeps its plain `access$get<X>$p`, so the
//! cross-file pass leaves every recorded name as this pass wrote it.

use super::member_names::vc_mangle;
use super::{erase, ir_method_desc, Under};
use crate::fx_hash::HashMap;
use crate::ir::{Callee, ExprId, IrExpr, IrFile};
use crate::jvm::private_static_access::member_property_accessor_name;
use crate::types::Ty;

pub(super) fn realize(ir: &mut IrFile, under: &Under, callable_under: &Under) {
    let calls = ir
        .private_property_accessor_calls
        .iter()
        .map(|(&call, accessor)| (call, accessor.clone()))
        .collect::<Vec<_>>();
    for (call, accessor) in calls {
        let parameters = ir
            .call_declared_params
            .get(&call)
            .expect("a private accessor call records its source arguments' declared parameters")
            .clone();
        let IrExpr::Call {
            callee: call_callee,
            ..
        } = &mut ir.exprs[call as usize]
        else {
            panic!("private accessor call {call} is not a call");
        };
        let (name, file_class) = match &mut **call_callee {
            Callee::Static {
                owner,
                name,
                descriptor,
                ..
            } => {
                let physical = std::iter::once(Ty::obj_name(*owner))
                    .chain(parameters.iter().map(|parameter| erase(parameter, under)))
                    .collect::<Vec<_>>();
                *descriptor = ir_method_desc(&physical, &erase(&accessor.result, under));
                (name, false)
            }
            Callee::CrossFile { name, .. } => (name, true),
            _ => {
                panic!("private accessor call {call} is neither a member static nor a facade call")
            }
        };
        if let Some(declared) = &accessor.mangled_accessor {
            let mangled = vc_mangle(
                declared,
                &parameters,
                &accessor.result,
                callable_under,
                file_class,
                false,
            );
            *name = member_property_accessor_name(&mangled, true);
        }
    }
}

/// Whether the cross-file pass keeps the name of the call at `index`: a private accessor call,
/// which `realize` already named. A field accessor's name carries no value-class hash.
pub(super) fn keeps_name(
    recorded: &HashMap<ExprId, crate::ir::PrivatePropertyAccessorCall>,
    index: usize,
) -> bool {
    ExprId::try_from(index).is_ok_and(|call| recorded.contains_key(&call))
}
