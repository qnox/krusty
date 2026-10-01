//! The reflection value a local delegated property's conventions receive.

use crate::types::Ty;

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
}
