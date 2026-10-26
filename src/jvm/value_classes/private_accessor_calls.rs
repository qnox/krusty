//! Physical names and descriptors of calls to a private member property's static accessor.
//!
//! Module realization records each such call with the dispatch receiver as the owner instance and
//! the source arguments' declared parameters in `call_declared_params`. The physical accessor takes
//! the owner first, then those parameters over their value-class carriers. An accessor the source
//! declares is named after that accessor's own JVM name, which kotlinc mangles for a value-class
//! signature (`access$getWeight-b--Cat4`); a field accessor keeps its plain `access$get<X>$p`.

use super::member_names::vc_mangle;
use super::{erase, ir_method_desc, Under};
use crate::ir::{Callee, IrExpr, IrFile};
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
        let Callee::Static {
            owner,
            name,
            descriptor,
            ..
        } = &mut **call_callee
        else {
            panic!("private accessor call {call} is not a static call");
        };
        if let Some(declared) = &accessor.mangled_accessor {
            let mangled = vc_mangle(
                declared,
                &parameters,
                &accessor.result,
                callable_under,
                false,
                false,
            );
            *name = member_property_accessor_name(&mangled, true);
        }
        let physical = std::iter::once(Ty::obj_name(*owner))
            .chain(parameters.iter().map(|parameter| erase(parameter, under)))
            .collect::<Vec<_>>();
        *descriptor = ir_method_desc(&physical, &erase(&accessor.result, under));
    }
}
