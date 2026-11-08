//! Enum classes, realized as ordinary common IR for targets with no class initialization of their
//! own.
//!
//! The checked IR leaves an enum's constants as a list on its class and its uses as four nodes:
//! [`IrExpr::EnumEntry`] (`Color.RED`), [`IrExpr::EnumValues`], [`IrExpr::EnumValueOf`] and
//! [`IrExpr::EnumEntries`]. None of it is target-specific, so this pass builds it once:
//!
//! - **Storage.** Each constant gets a static slot of this file, and the enum gets an
//!   `initialized` flag and, when this file reads it, a slot for its one `entries` list. The
//!   slots are private to the file, and an enum another file declares is not realized here, so
//!   a list no use of this file reads is never observed and is not built.
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
//! Another file reads a constant of an enum this file declares through an entry: an ordinary
//! function of this file answering the constant ([`publish_constants`]), which a target exports by
//! the enum and constant name, as it exports an object's instance entry. Only the declaring file
//! holds the constants' storage, so every read of a constant, in any file, finds the one object.
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
    /// The static slot of the enum's `entries` list, when this file reads it.
    entries: Option<u32>,
    /// Builds every constant, then `entries`, then the companion, when the enum is not built yet.
    initializer: FunId,
    /// `values()`: a fresh array of the constants.
    values: FunId,
    /// `valueOf(name)`: the constant of that name, or the target's failure.
    value_of: FunId,
}

/// Which official compiler's enum initialization a target follows. Kotlin leaves unspecified what
/// a constructor that reaches back into its own enum observes, and the two differ there; once the
/// enum is built they agree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Contract {
    /// kotlinc-native's `EnumClassLowering`: every constant is allocated and published before the
    /// first is constructed, each is then constructed in place; `values()` stores each constant
    /// at its current ordinal in name order (`valuesForEnum`); `entries` is built after the
    /// constants, so it is `null` inside a constructor.
    KotlinNative,
    /// kotlinc-js/wasm's enum lowerings: each constant is published once constructed, and a
    /// constant with a body right after its super call (`EnumEntryInstancesBodyLowering`);
    /// `values()` is an array of the constants as they stand, in declaration order; `entries` is
    /// built from `values()` on its first read.
    KotlinWasm,
}

/// Build the entry answering each constant of every enum `exports` publishes from `ir`, before
/// [`realize`] rewrites the entries' reads into reads of the constants' storage.
pub(crate) fn publish_constants(ir: &mut IrFile, exports: &mut super::DependencyExportSignatures) {
    let enums = exports.enums().to_vec();
    for class in enums {
        let classifier = ir.classes[class as usize].fq_name_id();
        let Some(result) = ir.declared_class_self_type(classifier) else {
            continue;
        };
        let names: Vec<Box<str>> = ir.classes[class as usize]
            .enum_entries
            .iter()
            .map(|entry| entry.name.as_str().into())
            .collect();
        for name in names {
            let value = ir.add_expr(crate::ir::IrExpr::EnumEntry {
                classifier,
                name: name.clone(),
            });
            ir.set_logical_type(value, result);
            let entry = super::construction_entries::entry(
                ir,
                &format!("<enum constant {name}>$get"),
                Vec::new(),
                result,
                value,
            );
            ir.construction_entries.insert(entry, class);
            exports.record_enum_constant_entry(super::EnumConstant { classifier, name }, entry);
        }
    }
}

/// Realize every enum class `ir` declares and rewrite every emitted use of one.
pub(crate) fn realize(ir: &mut IrFile, contract: Contract) {
    let enums: Vec<ClassId> = (0..ir.classes.len())
        .map(|class| ClassId::try_from(class).expect("too many classes"))
        .filter(|&class| is_enum(&ir.classes[class as usize]))
        .collect();
    let entries_read = uses::entries_read(ir);
    let mut realized: HashMap<TypeName, Realized> = HashMap::default();
    for class in enums {
        let reads_entries = entries_read.contains(&ir.classes[class as usize].fq_name_id());
        let enum_class = initializer::build(ir, class, reads_entries, contract);
        ir.enum_initializers.insert(class, enum_class.initializer);
        realized.insert(ir.classes[class as usize].fq_name_id(), enum_class);
    }
    if !realized.is_empty() {
        uses::rewrite(ir, &realized, contract);
    }
}

/// One of `kotlin.Enum`'s two properties, which a target answers from the storage its enum base
/// contributes to every constant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BaseMember {
    Name,
    Ordinal,
}

/// The `kotlin.Enum` property an external property read names: the getter frozen at the
/// frontend/backend boundary is declared by `kotlin.Enum`, under the name its provider decoded.
pub(crate) fn base_member(
    callables: &super::CheckedBackendCallables,
    target: crate::fir::ExternalPropertyId,
) -> Option<BaseMember> {
    let property = callables.property(target)?;
    let getter = callables.callable(property.getter)?;
    declared_base_member(getter.declaration_owner, &property.name)
}

/// Which `kotlin.Enum` property a getter with this checked semantic owner declares. The physical
/// owner never decides it: a JVM classpath realizes the getter on the mapped platform class, and a
/// facade or a getter without a published owner is not `kotlin.Enum`'s.
fn declared_base_member(
    owner: Option<crate::types::SemanticCallableOwner>,
    name: &str,
) -> Option<BaseMember> {
    let Some(crate::types::SemanticCallableOwner::Classifier(owner)) = owner else {
        return None;
    };
    if owner != crate::types::wk::kotlin_enum() {
        return None;
    }
    match name {
        "name" => Some(BaseMember::Name),
        "ordinal" => Some(BaseMember::Ordinal),
        _ => None,
    }
}

/// Whether `class` is an enum class: it extends `kotlin.Enum`. A constant's body subclass extends
/// its enum instead, so it is not one.
fn is_enum(class: &IrClass) -> bool {
    class.superclass == crate::types::wk::kotlin_enum()
}
