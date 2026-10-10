//! Parser-owned syntax retained for destructuring declarations across later compiler phases.

use super::{StmtId, TypeRef};
use std::collections::{HashMap, HashSet};

/// Whether a name-based entry names its property by the entry itself or by an explicit rename.
///
/// The parser records this from the presence of `=`. Later phases must not recover it by comparing
/// the entry name with the property name: an implicit `(val _)` and an explicit `(_ = _)` are
/// different syntax.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DestructureProperty {
    /// `(first)` or `(val first)`: the written name is the property.
    Implicit(String),
    /// `(name = property)`, including `(_ = property)`.
    Renamed(String),
}

impl DestructureProperty {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Implicit(name) | Self::Renamed(name) => name,
        }
    }
}

#[derive(Default)]
pub struct DestructuringSyntax {
    /// For a name-based destructuring statement, the source property each entry reads. `None`
    /// marks a positional (`componentN`) entry; absence means the whole statement is positional.
    pub source_properties: HashMap<u32, Vec<Option<DestructureProperty>>>,
    /// Destructuring statements prepended for lambda parameters. Their component calls have no
    /// source line of their own in kotlinc debug information.
    pub lambda_parameters: HashSet<StmtId>,
    /// Loops whose variable is a destructuring pattern, each with the destructuring statement
    /// prepended to its body, the only consumer of the synthetic loop variable.
    pub loops: HashMap<StmtId, StmtId>,
    /// Explicit type annotations on destructured bindings, parallel to the statement's entries.
    pub entry_types: HashMap<u32, Vec<Option<TypeRef>>>,
    /// Statements written in the parenthesized short form, whose entries carry no `val`/`var`:
    /// `val (a, b) = e`, `for ((a, b) in xs)`, and the lambda parameter `{ (a, b) -> }`. The
    /// square-bracket forms and the full form `(val a, val b) = e` are absent. Kotlin plans to give
    /// this syntax name-based meaning, so its positional entries are the subject of the
    /// `DeprecateNameMismatchInShortDestructuringWithParentheses` warnings.
    pub parenthesized_short_form: HashSet<StmtId>,
    /// `+DeprecateNameMismatchInShortDestructuringWithParentheses` without
    /// `+EnableNameBasedDestructuringShortForm`: positional entries of the parenthesized short form
    /// warn where the planned name-based reading would reject them or change their meaning.
    pub warns_parenthesized_short_form: bool,
}
