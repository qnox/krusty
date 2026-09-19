//! What the JVM found at the other end of each synthesized property reference.
//!
//! The common-IR [`crate::ir::PropRef`] says which property a reference names and what its
//! `KProperty` surface is. THIS table says what the JVM selected for it: the accessor names the
//! declaration carries, the exact module functions those accessors are, the physical return the
//! selected getter declares, which realization shape the selection landed on, and whether the
//! property is a value class's own storage. None of that is a Kotlin declaration fact, so none of
//! it belongs on the common record — and every one of them is an answer a later representation
//! pass would otherwise have to rebuild from a spelling or read back out of a rendered descriptor,
//! which is how this area produced references to methods that are declared nowhere.
//!
//! The key is the synthesized reference class's own internal name. It is minted exactly once, by
//! [`super::realize`], at the moment the reference is realized; nothing else can produce another
//! with it, and it survives every later pass because the class it names does.

use std::collections::HashMap;

use crate::ir::FunId;
use crate::types::{Ty, TypeName};

/// The physical accessor a property reference calls.
///
/// This is a JVM realization shape, not a Kotlin declaration kind: `Extension` and `AccessBridge`
/// are both static accessors taking their receiver first, and only the selection that made them
/// knows which is which. A pass that reads a neighbouring `Some`/`None` instead rebuilds an
/// extension's hash-mangled name for a member whose declaration is `-impl`-suffixed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PropertyAccessorRole {
    /// An instance accessor on the owner (`Z.getXx()`), or a static one on the file facade for a
    /// top-level property.
    Member,
    /// A static accessor on the declaring file's facade, taking the extension receiver first.
    Extension,
    /// A synthetic `access$…` on the owner, selected because the property (or its setter) is
    /// private and the reference is built outside it.
    AccessBridge,
}

/// The JVM's answer for one property reference.
#[derive(Clone, Debug)]
pub(crate) struct PropertyReferenceRealization {
    /// The accessor names the DECLARATION carries, before any `access$…` bridge is put in front of
    /// them and before any value-class mangling.
    ///
    /// Selection already resolved these — a `@get:JvmName("readY")` accessor is named `readY`
    /// here, whatever the property is spelled. A later pass that rebuilds them from the property's
    /// name discards that answer and names a method the declaration does not have.
    pub declared_getter_name: String,
    pub declared_setter_name: Option<String>,
    /// The referenced property's storage lives on a FILE FACADE and has no accessor of its own:
    /// the shape whose value-class-typed storage a later pass may realize over the carrier.
    ///
    /// Recorded from the selected declaration's own facts, so a reference from another file of the
    /// module answers the same as the declaration there. Joining the two sides by property
    /// spelling instead could not see a sibling file's declaration at all, and could match an
    /// unrelated same-spelled one.
    pub facade_storage: bool,
    pub accessor_role: PropertyAccessorRole,
    /// The exact module functions the selected accessors ARE, when this module declares them.
    ///
    /// This is the accessor's identity. The `access$…` bridge a private member is reached through
    /// exists for one exact method, and naming it by rebuilding a mangled spelling and looking for
    /// a method that answers to it makes a spelling the authority over a declaration.
    pub getter_function: Option<FunId>,
    pub setter_function: Option<FunId>,
    /// The PHYSICAL return the selected getter declares, whenever the declaration publishes one.
    ///
    /// A specialized generic property (`Pair<UInt, _>::first`) has semantic type `UInt` while its
    /// declaration still exposes `getFirst(): Object` — that object IS the box, and no value-class
    /// realization applies to it. Recording the declared return here is what lets a later pass
    /// tell the two apart without parsing the answer back out of a descriptor it rendered.
    ///
    /// The value-class pass updates this in step whenever it realizes the accessor over a carrier,
    /// so a reader always sees the return of the accessor the reference currently names.
    pub physical_getter_ret: Option<Ty>,
    /// The selected property IS the underlying storage of the value class that declares it.
    ///
    /// Reading it is the unbox itself, so its accessor stays an ordinary instance getter on the
    /// box (`Z.getX()I`) rather than a static realization over the carrier. A value class's
    /// underlying property has no distinguishing spelling, so this is taken from the declaration's
    /// own storage position — the class's sole field — or, for a dependency, from the underlying
    /// property its `@Metadata` publishes.
    pub declares_value_class_storage: bool,
}

/// Every property reference this file synthesized, by the reference class's internal name.
#[derive(Default)]
pub(crate) struct PropertyReferenceRealizations {
    by_reference: HashMap<TypeName, PropertyReferenceRealization>,
}

impl PropertyReferenceRealizations {
    pub(crate) fn record(
        &mut self,
        reference: TypeName,
        realization: PropertyReferenceRealization,
    ) {
        let previous = self.by_reference.insert(reference, realization);
        debug_assert!(
            previous.is_none(),
            "a property reference class is minted once, so it has one realization",
        );
    }

    pub(crate) fn get_mut(
        &mut self,
        reference: TypeName,
    ) -> Option<&mut PropertyReferenceRealization> {
        self.by_reference.get_mut(&reference)
    }

    /// Every module accessor any reference selected. A reader borrowing `ir.classes` mutably uses
    /// this to read those functions' spellings up front, by identity rather than by name.
    pub(crate) fn accessor_functions(&self) -> impl Iterator<Item = FunId> + '_ {
        self.by_reference
            .values()
            .flat_map(|realization| [realization.getter_function, realization.setter_function])
            .flatten()
    }
}
