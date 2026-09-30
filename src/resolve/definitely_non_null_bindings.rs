//! Call-binding refinement for definitely-non-null type-parameter intersections.

use std::collections::HashMap;

use crate::libraries::GenericSig;
use crate::symbol_resolver::GSigBinds;
use crate::types::Ty;

use super::Checker;

impl Checker<'_> {
    pub(super) fn tighten_definitely_non_null_bindings(
        &self,
        signature: &GenericSig,
        bindings: &mut GSigBinds,
        explicit_type_argument_count: usize,
        flow_intersections: &HashMap<&'static str, Vec<Ty>>,
    ) {
        crate::symbol_resolver::tighten_definitely_non_null_bindings(
            signature,
            bindings,
            explicit_type_argument_count,
            |actual, bound| {
                self.generic_bound_admits_with_flow_intersection(actual, bound, flow_intersections)
            },
        );
    }
}
