//! Value-class ABI names of generated in-place delegate methods.
//!
//! Delegate realization records each method with its exact getter identity. This pass extends the
//! getter's already-realized JVM name and then applies the delegate method's own value-class
//! signature mangle, without rediscovering a property from either spelling.

use super::member_names::vc_mangle;
use super::Under;
use crate::fx_hash::HashSet;
use crate::ir::IrFile;
use crate::types::Ty;

pub(super) fn rename(
    ir: &mut IrFile,
    renamed_functions: &mut HashSet<u32>,
    original_parameters: &[Vec<Ty>],
    original_returns: &[Ty],
    value_classes: &Under,
) {
    // An in-place delegate's `get<X>$delegate` method extends its getter's JVM name: when the
    // getter mangled to `getX-<hash>`, kotlinc names the method `getX-<hash>$delegate`, and a
    // value-class extension receiver then mangles that name again with the method's own signature.
    let renames = ir
        .jvm_delegate_methods_with_getters()
        .filter(|(_, getter)| renamed_functions.contains(getter))
        .map(|(method, getter)| {
            let base = format!("{}$delegate", ir.functions[getter as usize].name);
            let function = &ir.functions[method as usize];
            let name = vc_mangle(
                &base,
                &original_parameters[method as usize],
                &original_returns[method as usize],
                value_classes,
                function.dispatch_receiver.is_none(),
                false,
            );
            (method, name)
        })
        .collect::<Vec<_>>();
    for (method, name) in renames {
        ir.functions[method as usize].name = name;
        renamed_functions.insert(method);
    }
}
