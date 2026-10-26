//! The JVM carrier of every type-parameter occurrence.
//!
//! kotlinc erases a type parameter to its `erasedUpperBound`: the first written bound that is a
//! class, otherwise the first written bound. For `class Holder<T> where T : Marker, T : Base` that is
//! `Base`, in the constructor, the field, the getter, every member and inner-class slot, and every
//! call to them from another file. Common IR keeps the written bounds on the declaration and a
//! representative bound on each occurrence. This pass selects the carrier from the declaration's
//! complete bounds, the records the file carries for its own declarations and copies with the
//! module declarations it calls, and gives it to every occurrence. The occurrence keeps its
//! identity, so generic signatures and metadata still name the parameter, and the declaration
//! keeps its bounds in written order.

use crate::fx_hash::HashMap;
use crate::ir::IrFile;

pub(super) fn realize(ir: &mut IrFile) {
    let mut carriers = HashMap::default();
    for (parameter, bounds) in ir.type_parameter_declarations() {
        let Some(primary) = super::generic_erasure::primary_bound(bounds) else {
            continue;
        };
        // The first written bound is already the representative an occurrence carries.
        if primary != 0 {
            carriers.insert(parameter.to_owned(), bounds[primary].0);
        }
    }
    ir.rebind_type_parameter_carriers(&carriers);
}
