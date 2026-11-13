//! Declaration-owned association between one serialized accessor and its normalized property.

/// The declared semantic signature of a dependency getter. It carries no physical ABI or
/// implementation choice; consumers authenticate the property and class before realizing it.
#[derive(Clone, Copy)]
pub struct DependencyGetterSignature<'a> {
    pub owner: crate::types::TypeName,
    pub name: &'a str,
    pub parameters: &'a [crate::types::Ty],
    pub result: crate::types::Ty,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KlibAccessorRole {
    Getter,
    Setter,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KlibBodyAccessor {
    pub(crate) property: crate::fir::ExternalPropertyId,
    pub(crate) role: KlibAccessorRole,
    pub(crate) name: Box<str>,
}

impl KlibBodyAccessor {
    /// Whether the accessor is its property's setter rather than its getter.
    pub fn is_setter(&self) -> bool {
        self.role == KlibAccessorRole::Setter
    }
}
