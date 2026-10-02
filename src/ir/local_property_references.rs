//! The reflection value a local delegated property's conventions receive.

use crate::types::{Ty, TypeName};

use super::IrModuleSource;

/// Backend-neutral reflection value passed to a local delegated property's conventions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrLocalPropertyReference {
    pub name: Box<str>,
    pub property_type: Ty,
    /// The local delegated property's declaration identity, as the checker allocated it. Every
    /// reference of one declaration carries it, in whichever function its read or write is
    /// lowered; two declarations never share it, whatever their names. A backend that realizes
    /// one reflection object per property keys it by this, never by `name` or by an origin.
    pub declaration: crate::fir::LocalDelegatedPropertyId,
    pub mutable: bool,
    /// The class lexically declaring it, or `None` in a top-level declaration, whose container is
    /// `source`'s file facade.
    pub class: Option<TypeName>,
    pub source: IrModuleSource,
    /// Its position among the class's local delegated properties, in source order.
    pub ordinal: u32,
    /// The source order of the member declaration whose body declares it.
    pub member_order: u32,
}
