//! The static function a call to a value-class member reaches once the class is realized over
//! its carrier.

use krusty::ir::IrFile;

/// The static function realizing `methods[index]` of value class `class`, if it has one.
///
/// A user-written `Any` override is itself turned into the static `toString-impl`,
/// `hashCode-impl` or `equals-impl`. The `toString`/`hashCode`/`equals` the class generates is
/// realized as the box's delegator to the generated static `-impl`; like kotlinc, a call naming it
/// calls that `-impl` over the receiver's carrier.
pub(super) fn realized_static(ir: &IrFile, class: u32, index: u32) -> Option<u32> {
    let methods = &ir.classes[class as usize].methods;
    let function = *methods.get(index as usize)?;
    if ir.functions.get(function as usize)?.is_static {
        return Some(function);
    }
    let member = ir.jvm_value_class_any_delegators.get(&function)?;
    methods
        .iter()
        .copied()
        .find(|candidate| ir.jvm_value_class_generated_any.get(candidate) == Some(member))
}
