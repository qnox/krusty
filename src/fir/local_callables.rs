//! Identities of the callables a body declares inside itself.
//!
//! A local function is a declaration with no place in the module's declaration stream: it belongs
//! to one body, is reachable only from it, and must still be nameable across two parses of that
//! body. These identities are what make it so without retaining a single syntax coordinate.

use super::BodyOwnerId;

/// Stable identity of a local function within one freshly parsed source declaration stream.
///
/// This is deliberately not a parser-arena id or a source range. The body checker assigns the
/// ordinal from the local-function declaration stream on both parses, allowing a retained inline
/// FIR body to name one of its own local callables without retaining syntax coordinates.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BodyLocalCallableDeclarationId {
    owner: BodyOwnerId,
    ordinal: u32,
    role: BodyLocalCallableRole,
}

/// What a body-local callable declares, and so which declaration stream its ordinal counts: a
/// source local function among the body's local functions, or an accessor kotlinc generates for a
/// local delegated property among the body's local delegated properties.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum BodyLocalCallableRole {
    Function,
    DelegateGetter,
    DelegateSetter,
}

impl BodyLocalCallableDeclarationId {
    pub(crate) const fn with_role(
        owner: BodyOwnerId,
        ordinal: u32,
        role: BodyLocalCallableRole,
    ) -> Self {
        Self {
            owner,
            ordinal,
            role,
        }
    }
}
