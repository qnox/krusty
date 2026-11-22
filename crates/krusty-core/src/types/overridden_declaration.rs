use super::{Ty, TypeName};

/// Exact semantic identity of a declaration that a selected callable overrides.
///
/// The core member hierarchy records these on the selected declaration from the override edges it
/// already proves (owner subtyping, the complete input shape, and the result). A target compares
/// them to a declaration it implements specially, such as `kotlin/Any.toString(): String`; it never
/// infers an override from a member's spelling or shape alone.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct OverriddenDeclaration {
    /// The classifier that declares the overridden member.
    pub owner: TypeName,
    pub name: Box<str>,
    /// Declared extension receiver of a member extension; `None` for an ordinary member.
    pub receiver: Option<Ty>,
    /// Declared parameter types before any use-site substitution.
    pub params: Box<[Ty]>,
    /// Declared result type before any use-site substitution.
    pub ret: Ty,
}
