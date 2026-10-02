//! Declaration provenance attached to a resolved classifier facet, including typealias expansion.

use crate::types::{Ty, TypeName};

/// A classpath typealias expansion as a selected use site needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct AliasExpansion {
    /// Stable qualified identity of the alias declaration. Source spelling is resolved to this
    /// identity before the template is selected.
    pub identity: TypeName,
    /// The alias's target classifier. A template applies only when the spelling that named it
    /// actually resolved to this classifier.
    pub target: TypeName,
    /// The alias's own type-parameter names, in declaration order: the substitution domain.
    pub formals: Vec<String>,
    /// The target applied to its arguments, with alias parameters represented as `Ty::TyParam`.
    pub expansion: Ty,
    /// Source spellings inherited by arguments in the expansion.
    pub expansion_spelling: crate::spelling::Spelled,
}

/// A provider may normalize an ordinary platform declaration onto a common semantic classifier
/// (for example a JVM class onto its Kotlin classifier). That is distinct from a source typealias:
/// consumers use this tagged provenance instead of inferring alias-ness from differing declaration
/// and classifier identities.
#[derive(Clone, Debug, PartialEq)]
pub enum ClassifierDeclaration {
    Ordinary(TypeName),
    TypeAlias(AliasExpansion),
}
