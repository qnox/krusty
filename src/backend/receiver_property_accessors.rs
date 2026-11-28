//! The accessors of a property that take more than a dispatch receiver: an extension property
//! (`val Foo.bar get() = …`), or one with context parameters.
//!
//! Such a property has no storage another file could reach: an extension property cannot have a
//! backing field, and a context property's getter is always declared. Every access is a call of
//! the accessor the checked lowering built, with Kotlin's declared operand order: the context
//! parameters, then the extension receiver, then a setter's value. A member's accessor is an
//! instance method of its owner, taking the dispatch receiver first.
//!
//! The defining file reads that shape from its checked layout and a using file from the module
//! record. Both are copied from one declaration header, so a target that links the two files
//! through one entry point per accessor derives that entry's signature the same way on both sides.

use crate::ir::{FunId, IrLocalPropertyLayout, IrModuleProperty, IrStaticPlacement};
use crate::types::{Ty, TypeName};

/// The accessors a property with a receiver or context parameters is read and written through, as
/// its defining file's checked layout records them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiverAccessors {
    pub getter: FunId,
    pub setter: Option<FunId>,
    /// The classifier whose instance method each accessor is; `None` for a package property.
    pub owner: Option<TypeName>,
}

impl ReceiverAccessors {
    /// The accessors of `layout`, or why it is not a property realized through receiver accessors.
    pub fn of_layout(layout: &IrLocalPropertyLayout) -> Result<Self, &'static str> {
        match layout {
            IrLocalPropertyLayout::TopLevelAccessor { getter, setter, .. } => Ok(Self {
                getter: *getter,
                setter: *setter,
                owner: None,
            }),
            IrLocalPropertyLayout::MemberExtension {
                owner,
                getter,
                setter,
                ..
            } => Ok(Self {
                getter: *getter,
                setter: *setter,
                owner: Some(*owner),
            }),
            // A member with a receiver beyond its owner's instance has context parameters, and a
            // context property always declares its getter.
            IrLocalPropertyLayout::Member {
                owner,
                getter,
                setter,
                ..
            } => Ok(Self {
                getter: getter.ok_or("a context property with no getter")?,
                setter: *setter,
                owner: Some(*owner),
            }),
            IrLocalPropertyLayout::TopLevelStorage { .. } => {
                Err("a stored property reached through a receiver")
            }
        }
    }

    /// The accessor a read, or with `writing` a write, calls.
    pub fn accessor(&self, writing: bool) -> Result<FunId, &'static str> {
        match writing {
            false => Ok(self.getter),
            true => self.setter.ok_or("a write to a read-only property"),
        }
    }
}

/// The declared operands of a locally laid-out property's accessors beyond a dispatch receiver,
/// in Kotlin's order: the context parameters, then the extension receiver. Empty for a property
/// whose accessors take none.
pub fn layout_parameters(layout: &IrLocalPropertyLayout) -> Vec<Ty> {
    match layout {
        IrLocalPropertyLayout::TopLevelStorage { .. } => Vec::new(),
        IrLocalPropertyLayout::TopLevelAccessor {
            receiver,
            context_parameters,
            ..
        } => context_parameters
            .iter()
            .copied()
            .chain(*receiver)
            .collect(),
        IrLocalPropertyLayout::Member {
            context_parameters, ..
        } => context_parameters.clone(),
        IrLocalPropertyLayout::MemberExtension {
            receiver,
            context_parameters,
            ..
        } => context_parameters
            .iter()
            .copied()
            .chain(std::iter::once(*receiver))
            .collect(),
    }
}

/// The same operands for a property another file of the module declares, from its module record.
///
/// A companion-associated property's recorded extension receiver (a `companion { }` block member,
/// or a companion extension `companion val C.p`) is its classifier: a name-resolution coordinate,
/// not a parameter. Its accessors take no receiver value, as the defining file's layout records.
pub fn record_parameters(record: &IrModuleProperty) -> Vec<Ty> {
    let associated = record.companion_associated
        || matches!(record.placement, IrStaticPlacement::CompanionBlock { .. });
    let extension_receiver = record.extension_receiver.filter(|_| !associated);
    record
        .context_parameters
        .iter()
        .copied()
        .chain(extension_receiver)
        .collect()
}
