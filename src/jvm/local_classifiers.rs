//! Classifiers declared in executable code, and those nested in them. JVM emission gives them LOCAL
//! metadata visibility, local class ids and no nullability annotations.

use std::collections::HashSet;

use crate::ir::{IrClass, IrFile};
use crate::types::TypeName;

/// kotlinc's `IrClass.isLocal` for a declared classifier: a class declared in executable code (a
/// local class or an anonymous object) or nested, at any depth, in one. kotlinc's lambda and
/// callable-reference classes are local too; their writers here annotate nothing to begin with.
pub(super) fn is_local(ir: &IrFile, class: &IrClass) -> bool {
    class.is_local_class
        || class.is_anonymous_object
        || class
            .fq_name_id()
            .existing_nested_owners()
            .into_iter()
            .find_map(|owner| ir.class_id_by_name(owner))
            .is_some_and(|owner| is_local(ir, &ir.classes[owner as usize]))
}

/// Every classifier of the file for which [`is_local`] holds. `@Metadata` names them by
/// their raw internal name, marked local in the string table.
pub(super) fn names(ir: &IrFile) -> HashSet<TypeName> {
    ir.classes
        .iter()
        .filter(|class| is_local(ir, class))
        .map(|class| class.fq_name_id())
        .collect()
}
