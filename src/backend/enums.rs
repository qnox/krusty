//! Enum classes, realized as ordinary common IR for targets with no class initialization of their
//! own.
//!
//! The checked IR leaves an enum's constants as a list on its class and its uses as four nodes:
//! [`IrExpr::EnumEntry`] (`Color.RED`), [`IrExpr::EnumValues`], [`IrExpr::EnumValueOf`] and
//! [`IrExpr::EnumEntries`]. None of it is target-specific, so this pass builds it once:
//!
//! - **Storage.** Each constant gets a static slot of this file, and the enum gets an
//!   `initialized` flag and a slot for its one `entries` list. The slots are private to the file;
//!   an enum another file declares is not realized here.
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
//!   order. `entries` is a getter. Every enum has its list, whether or not a use reads it, so what
//!   a program observes does not depend on which file reads it first. Under
//!   [`Contract::KotlinNative`] the initializer builds the list after the constants, as
//!   Kotlin/Native does, so the getter runs the initializer and reads it, still `null` from inside
//!   a constant's construction. Under [`Contract::KotlinWasm`] the getter builds it on its first
//!   read, as kotlinc-wasm does, by calling the stdlib's `enumEntries(Array)` with the constants:
//!   that call is an ordinary dependency call, whose library body makes the stdlib's own list.
//!
//! What stays with the target is representation: allocating a constant, the `kotlin.Enum`
//! storage of `name` and `ordinal` ([`IrExpr::EnumConstant`]) and its reads
//! ([`crate::ir::IrIntrinsic::EnumOrdinal`]), making Kotlin/Native's `entries` list
//! ([`IrExpr::EnumEntriesList`]), the failure of `valueOf` ([`IrExpr::NoSuchEnumConstant`]), and,
//! for a target whose companion getter runs before the companion is built, calling the enum's
//! initializer there ([`IrFile::enum_initializers`]).
//!
//! Another file reads a constant of an enum this file declares through an entry: an ordinary
//! function of this file answering the constant ([`publish_constants`]), which a target exports by
//! the enum and constant name, as it exports an object's instance entry. Only the declaring file
//! holds the constants' storage, so every read of a constant, in any file, finds the one object.
//! `values()`, `valueOf` and `entries` read that storage too, so the functions realizing them are
//! entries of the same kind, published by the enum and the member ([`realize`]): another file's
//! use of any of them is a call of the declaring file's function.
//!
//! A file may read a constant, or use `values()`, `valueOf` or `entries`, before the file
//! declaring its enum is lowered. Both files link the use by the enum's identity, as they link an object's instance
//! (see [`super::instance_links`]), so the order of files does not matter.
//!
//! A target runs this before the shared default-argument lowering, which then realizes a
//! constant that leaves constructor arguments out like any other construction. The JVM keeps
//! kotlinc's `<clinit>`, `$VALUES` and `$ENTRIES` and does not run it. An enum that another file
//! declares keeps its use nodes, `enumEntries<E>()` among them as [`IrExpr::EnumEntries`], which
//! a target lowers as calls of the linked entries ([`StaticMember::of`]).

mod initializer;
#[cfg(test)]
mod tests;
mod uses;

pub(super) use uses::emitted_use_roots;

use crate::fx_hash::HashMap;
use crate::ir::{Callee, ClassId, FunId, IrClass, IrExpr, IrFile};
use crate::types::TypeName;

/// What one enum class of this file was realized as.
#[derive(Clone, Debug)]
struct Realized {
    /// The static slot of each constant, in declaration order.
    slots: Vec<u32>,
    /// Builds every constant, then `entries` under [`Contract::KotlinNative`], then the companion,
    /// when the enum is not built yet.
    initializer: FunId,
    /// `values()`: a fresh array of the constants.
    values: FunId,
    /// `valueOf(name)`: the constant of that name, or the target's failure.
    value_of: FunId,
    /// The `entries` getter: the enum's one list.
    entries: FunId,
}

/// Which official compiler's enum initialization a target follows. Kotlin leaves unspecified what
/// a constructor that reaches back into its own enum observes, and the two differ there; once the
/// enum is built they agree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Contract {
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

impl Contract {
    /// Whether an enum's `entries` is built by calling the selected `enumEntries(Array)`, as the
    /// synthetic body states, rather than by the runtime's own list.
    pub fn builds_entries_by_factory(self) -> bool {
        match self {
            Self::KotlinNative => false,
            Self::KotlinWasm => true,
        }
    }
}

/// Build the entry answering each constant of every enum `ir` declares, before [`realize`] rewrites
/// the entries' reads into reads of the constants' storage. Every enum gets them, whether its
/// module is a library or the program: another file of the module, as much as another module, may
/// read any of its constants.
pub fn publish_constants(ir: &mut IrFile, exports: &mut super::DependencyExportSignatures) {
    let enums: Vec<ClassId> = (0..ir.classes.len())
        .map(|class| ClassId::try_from(class).expect("too many classes"))
        .filter(|&class| is_enum(&ir.classes[class as usize]))
        .collect();
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
            let entry = ir.add_generated_entry(
                &format!("<enum constant {name}>$get"),
                Vec::new(),
                result,
                value,
            );
            ir.construction_entries.insert(entry, class);
            exports.record_instance_entry(
                super::instance_links::InstanceEntry::EnumConstant(super::EnumConstant {
                    classifier,
                    name,
                }),
                entry,
            );
        }
    }
}

/// One of the members of an enum class that answer its constants and that another file calls
/// through the declaring file's entry (see [`super::instance_links`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StaticMember {
    /// `values()`: a fresh array of the constants.
    Values,
    /// `valueOf(name)`: the constant of that name, or the target's failure.
    ValueOf,
    /// `entries`: the enum's one list of its constants.
    Entries,
}

impl StaticMember {
    /// How Kotlin source spells the member, for a diagnostic.
    pub fn source_name(self) -> &'static str {
        match self {
            Self::Values => "`values()`",
            Self::ValueOf => "`valueOf`",
            Self::Entries => "`entries`",
        }
    }

    /// The member another file reads through the declaring file's entry, when `expression` names
    /// one of an enum: `values()`, `valueOf` and `entries` (`E.entries`, and `enumEntries<E>()`
    /// once [`realize`] has named the enum).
    pub fn of(expression: &IrExpr) -> Option<(TypeName, Self)> {
        match expression {
            IrExpr::EnumValues { classifier } => Some((*classifier, Self::Values)),
            IrExpr::EnumValueOf { classifier, .. } => Some((*classifier, Self::ValueOf)),
            IrExpr::EnumEntries { classifier } => Some((*classifier, Self::Entries)),
            _ => None,
        }
    }
}

/// The function an enum's `entries` getter builds its list by on [`Contract::KotlinWasm`]: the
/// stdlib's `enumEntries(Array)` that resolution selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntriesFactory {
    /// The selected dependency declaration, which an earlier unit emits.
    Selected(crate::fir::EnumEntriesFactory),
    /// This IR's own function lowering that declaration's body: a dependency support unit that
    /// emits `enumEntries` and materializes a library enum calls the function it publishes, which
    /// no earlier unit exports.
    Own(FunId),
}

impl EntriesFactory {
    /// `factory`, whose provider-assigned declaration is `identity`, as `ir` reaches it: the
    /// function `exports` publishes under that identity, when this unit lowers it, and otherwise
    /// the selected declaration.
    pub fn bind(
        factory: crate::fir::EnumEntriesFactory,
        identity: Option<&crate::libraries::KlibDependencyCallable>,
        exports: &super::DependencyExportSignatures,
    ) -> Self {
        exports
            .callables()
            .find(|(_, exported)| Some(*exported) == identity)
            .map_or(Self::Selected(factory), |(function, _)| Self::Own(function))
    }
}

/// Realize every enum class `ir` declares and rewrite every emitted use of one. Every enum's
/// `values()`, `valueOf` and `entries` are published as entries, whether its module is a library
/// or the program, as its constants are ([`publish_constants`]). Under [`Contract::KotlinWasm`]
/// `entries` is built by `factory`, the stdlib's `enumEntries(Array)` resolution selected; there
/// is none to build it by when the platform declares no such function.
pub fn realize(
    ir: &mut IrFile,
    contract: Contract,
    factory: Option<EntriesFactory>,
    exports: &mut super::DependencyExportSignatures,
) -> Result<(), String> {
    let enums: Vec<ClassId> = (0..ir.classes.len())
        .map(|class| ClassId::try_from(class).expect("too many classes"))
        .filter(|&class| is_enum(&ir.classes[class as usize]))
        .collect();
    let entries_list = if contract.builds_entries_by_factory() {
        factory.map(initializer::EntriesList::Factory)
    } else {
        Some(initializer::EntriesList::Runtime)
    };
    let mut realized: HashMap<TypeName, Realized> = HashMap::default();
    for class in enums {
        let entries_list = entries_list.ok_or(
            "the platform declares no `kotlin.enums.enumEntries(Array)` to build `entries` by",
        )?;
        let enum_class = initializer::build(ir, class, contract, entries_list);
        ir.enum_initializers.insert(class, enum_class.initializer);
        let classifier = ir.classes[class as usize].fq_name_id();
        for (member, entry) in [
            (StaticMember::Values, enum_class.values),
            (StaticMember::ValueOf, enum_class.value_of),
            (StaticMember::Entries, enum_class.entries),
        ] {
            exports.record_instance_entry(
                super::instance_links::InstanceEntry::EnumMember { classifier, member },
                entry,
            );
        }
        realized.insert(classifier, enum_class);
    }
    uses::rewrite(ir, &realized);
    Ok(())
}

/// One of `kotlin.Enum`'s two properties, which a target answers from the storage its enum base
/// contributes to every constant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseMember {
    Name,
    Ordinal,
}

/// The `kotlin.Enum` property an external property read names: the getter frozen at the
/// frontend/backend boundary is declared by `kotlin.Enum`, under the name its provider decoded.
pub fn base_member(
    callables: &super::CheckedBackendCallables,
    target: crate::fir::ExternalPropertyId,
) -> Option<BaseMember> {
    let property = callables.property(target)?;
    let getter = callables.callable(property.getter)?;
    declared_base_member(getter.declaration_owner, &property.name)
}

/// The `kotlin.Enum` property the dependency getter `getter` declares. A target answering `name`
/// and `ordinal` from its enum base realizes a library body's call of that getter as it does a
/// source read, in place of the getter's KLIB body, which reads a class layout the target does not
/// give its enums.
pub fn getter_base_member(
    getter: crate::libraries::DependencyGetterSignature<'_>,
) -> Option<BaseMember> {
    if !getter.parameters.is_empty() {
        return None;
    }
    declared_base_member(
        Some(crate::types::SemanticCallableOwner::Classifier(
            getter.owner,
        )),
        getter.name,
    )
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

/// Leave out the `entries` getter of every enum no emitted body of the program reads, when the
/// getter builds its list by calling the stdlib's `enumEntries` ([`Contract::builds_entries_by_factory`]).
/// An unread getter is not an emitted body, so the dependency support its call selects is not an
/// emitted use either: a program that never reads `entries` does not carry the stdlib's list
/// classes. A read in any file of the program, whether of an enum that file declares or another
/// file's, keeps the getter; the entry is withdrawn with it.
pub fn withdraw_unread_entries(
    files: &mut [(&mut IrFile, &mut super::DependencyExportSignatures)],
) {
    let mut read: crate::fx_hash::HashSet<TypeName> = crate::fx_hash::HashSet::default();
    for (ir, _) in files.iter() {
        read.extend(ir.exprs.iter().filter_map(|node| match node {
            IrExpr::EnumEntries { classifier } => Some(*classifier),
            _ => None,
        }));
    }
    for (ir, exports) in files.iter_mut() {
        let getters: Vec<(TypeName, FunId)> = exports
            .instance_entries()
            .iter()
            .filter_map(|(instance, getter)| match instance {
                super::instance_links::InstanceEntry::EnumMember {
                    classifier,
                    member: StaticMember::Entries,
                } => Some((*classifier, *getter)),
                _ => None,
            })
            .collect();
        for (classifier, getter) in getters {
            let called = ir.exprs.iter().any(|node| {
                matches!(node, IrExpr::Call { callee, .. }
                    if matches!(&**callee, Callee::Local(function) if *function == getter))
            });
            if read.contains(&classifier) || called {
                continue;
            }
            ir.inline_only_fns.insert(getter);
            exports.withdraw_instance_entry(&super::instance_links::InstanceEntry::EnumMember {
                classifier,
                member: StaticMember::Entries,
            });
        }
    }
}
