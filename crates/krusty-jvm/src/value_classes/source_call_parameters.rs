//! The declared parameters a call to a same-file or sibling-source function supplies.
//!
//! A call whose callee is a function of this file takes its parameter vector from that function's
//! declaration; a sibling-source default call takes its provider's. A call that omits defaulted
//! parameters supplies only the rest, and a default stub receives the parameters it boxes as
//! nullable.

use krusty::ir::{Callee, IrFile};
use krusty::types::Ty;

pub(super) fn supplied(ir: &IrFile, callee: &Callee, declared: &[Vec<Ty>]) -> Option<Vec<Ty>> {
    if let Callee::ModuleWithDefaults {
        target,
        default_provider,
        defaults,
        ..
    } = callee
    {
        // A sibling-source default call supplies only the provider declaration parameters that
        // remain after removing the omitted ordinals.
        return Some(
            super::default_calls::supplied_provider_parameters(
                ir,
                *target,
                *default_provider,
                defaults,
            )
            .collect(),
        );
    }
    let function = callee.source_function()?;
    let mut parameters = declared[function as usize].clone();
    if matches!(
        callee,
        Callee::LocalDefault(_)
            | Callee::ClassStaticDefault { .. }
            | Callee::LocalWithDefaults { .. }
            | Callee::ClassStaticWithDefaults { .. }
    ) {
        if let Some(boxed) = ir.default_stub_boxed_params.get(&function) {
            for &(index, ty) in boxed {
                if let Some(parameter) = parameters.get_mut(index) {
                    // `Target::Boxed` is the semantic representation boundary. The physical
                    // descriptor remains the non-null box type recorded in
                    // `default_stub_boxed_params`.
                    *parameter = Ty::nullable(ty);
                }
            }
        }
    }
    let omitted = match callee {
        Callee::LocalWithDefaults { defaults, .. }
        | Callee::ClassStaticWithDefaults { defaults, .. } => Some(defaults.as_ref()),
        _ => None,
    };
    Some(
        parameters
            .into_iter()
            .enumerate()
            .filter_map(|(parameter, ty)| {
                omitted
                    .is_none_or(|defaults| !defaults.contains(&(parameter as u32)))
                    .then_some(ty)
            })
            .collect(),
    )
}
