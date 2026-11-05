//! Enum classes, realized as ordinary common IR for targets with no class initialization of their
//! own.
//!
//! The checked IR leaves an enum's constants as a list on its class and its uses as four nodes:
//! [`IrExpr::EnumEntry`] (`Color.RED`), [`IrExpr::EnumValues`], [`IrExpr::EnumValueOf`] and
//! [`IrExpr::EnumEntries`]. None of it is target-specific, so this pass builds it once:
//!
//! - **Storage.** Each constant gets a static slot of this file, and the enum gets an
//!   `initialized` flag and a slot for its one `entries` list.
//! - **The initializer.** Kotlin initializes an enum class as a whole: its constants, then the
//!   `entries` list, then the companion object. One function per enum does that when its flag is
//!   clear, in Kotlin/Native's order (its `EnumClassLowering`): every constant is first allocated
//!   and published to its slot ([`IrExpr::EnumAllocation`]), and only then is each constructed in
//!   place, in declaration order. A constructor that reaches back into its enum therefore finds
//!   every constant, the ones not constructed yet with zero fields and `kotlin.Enum` storage
//!   unset. The flag is set BEFORE anything is built, so such a back-edge finds the work under
//!   way instead of starting it again. A constant whose construction throws leaves the enum
//!   unbuilt: the flag and every slot are cleared and the exception rethrown, so the next access
//!   builds it again.
//! - **Uses.** Reading a constant runs the initializer and reads its slot. `values()` is a
//!   function returning a fresh array, which is Kotlin's contract: a caller may write to it. It
//!   places each constant at its current `ordinal`, visiting them in name order, as
//!   Kotlin/Native's `valuesForEnum` does; once the enum is built that is declaration order.
//!   `valueOf` is a function comparing its argument with each constant's name in declaration
//!   order. `entries` runs the initializer and reads the list, which is still `null` when read
//!   from inside a constant's construction, as on Kotlin/Native.
//!
//! What stays with the target is representation: allocating a constant, the `kotlin.Enum`
//! storage of `name` and `ordinal` ([`IrExpr::EnumConstant`]) and its reads
//! ([`crate::ir::IrIntrinsic::EnumOrdinal`]), making the `entries` list
//! ([`IrExpr::EnumEntriesList`]), the failure of `valueOf` ([`IrExpr::NoSuchEnumConstant`]), and,
//! for a target whose companion getter runs before the companion is built, calling the enum's
//! initializer there ([`IrFile::enum_initializers`]).
//!
//! A target runs this before the shared default-argument lowering, which then realizes a
//! constant that leaves constructor arguments out like any other construction. The JVM keeps
//! kotlinc's `<clinit>`, `$VALUES` and `$ENTRIES` and does not run it. An enum that another file
//! declares keeps its use nodes; the target declines them by name.

mod initializer;
#[cfg(test)]
mod tests;
mod uses;

use crate::fx_hash::HashMap;
use crate::ir::{ClassId, FunId, IrClass, IrFile};
use crate::types::TypeName;

/// What one enum class of this file was realized as.
#[derive(Clone, Debug)]
struct Realized {
    /// The static slot of each constant, in declaration order.
    slots: Vec<u32>,
    /// The static slot of the enum's `entries` list.
    entries: u32,
    /// Builds every constant, then `entries`, then the companion, when the enum is not built yet.
    initializer: FunId,
    /// `values()`: a fresh array of the constants.
    values: FunId,
    /// `valueOf(name)`: the constant of that name, or the target's failure.
    value_of: FunId,
}

/// Realize every enum class `ir` declares and rewrite every emitted use of one.
pub(crate) fn realize(ir: &mut IrFile) {
    let enums: Vec<ClassId> = (0..ir.classes.len())
        .map(|class| ClassId::try_from(class).expect("too many classes"))
        .filter(|&class| is_enum(&ir.classes[class as usize]))
        .collect();
    let mut realized: HashMap<TypeName, Realized> = HashMap::default();
    for class in enums {
        let enum_class = initializer::build(ir, class);
        ir.enum_initializers.insert(class, enum_class.initializer);
        realized.insert(ir.classes[class as usize].fq_name_id(), enum_class);
    }
    if !realized.is_empty() {
        uses::rewrite(ir, &realized);
    }
}

/// Whether `class` is an enum class: it extends `kotlin.Enum`. A constant's body subclass extends
/// its enum instead, so it is not one.
fn is_enum(class: &IrClass) -> bool {
    class.superclass == crate::types::wk::kotlin_enum()
}
