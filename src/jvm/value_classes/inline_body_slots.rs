//! The slot types a lambda's retained inline body is lowered against.
//!
//! A lambda's implementation method receives its own value-class parameters boxed through the
//! `Object` slots of `FunctionN.invoke`. Its retained inline body does not cross that boundary: like
//! the lambda kotlinc inlines, it takes each parameter unboxed, and whoever inlines it unboxes the
//! incoming box once at the `invoke` (`StackValue.coerce`).

use std::collections::HashMap;

use crate::ir::IrFile;
use crate::types::Ty;

/// Each lambda's own parameters as the lambda declares them, by the slot its bodies read them from.
pub(super) struct OwnParameters(HashMap<u32, Vec<(u32, Ty)>>);

/// The declared types of every lambda's own parameters, read before value-class lowering boxes the
/// implementation's.
pub(super) fn own_parameters(ir: &IrFile) -> OwnParameters {
    let parameters = ir
        .lambda_own_params_from
        .iter()
        .map(|(&function, &own_from)| {
            let declaration = &ir.functions[function as usize];
            let base = u32::from(declaration.dispatch_receiver.is_some() && !declaration.is_static);
            let own = declaration
                .params
                .iter()
                .enumerate()
                .skip(own_from as usize)
                .map(|(index, &ty)| (base + index as u32, ty))
                .collect();
            (function, own)
        })
        .collect();
    OwnParameters(parameters)
}

impl OwnParameters {
    /// `slots`, the slot types of the lambda `function`'s implementation, with its own parameters
    /// as the lambda declares them.
    pub(super) fn unboxed(&self, function: u32, mut slots: HashMap<u32, Ty>) -> HashMap<u32, Ty> {
        for &(slot, ty) in self.0.get(&function).into_iter().flatten() {
            slots.insert(slot, ty);
        }
        slots
    }
}
