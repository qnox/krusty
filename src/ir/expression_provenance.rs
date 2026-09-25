//! Stable source distinctions retained beside otherwise-generic expression nodes.

/// Which declaration a checked enum `valueOf` operation selected. Both name the same lookup by
/// entry name; they are different declarations, and only one of them is `inline`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnumValueOfDeclaration {
    /// The classifier's own implicit member — `E.valueOf(name)`.
    Member,
    /// The standard library's top-level `enumValueOf<E>(name)`, whose body expands at the call.
    StandardLibraryTopLevel,
}

/// Source Boolean operator represented by a generic [`super::IrExpr::When`] after common lowering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrShortCircuitKind {
    And,
    Or,
}
