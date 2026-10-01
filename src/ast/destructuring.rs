//! Parser-owned syntax retained for destructuring declarations across later compiler phases.

use super::{StmtId, TypeRef};
use crate::diag::Span;
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub struct DestructuringSyntax {
    /// For a name-based destructuring statement, the source property each entry reads. `None`
    /// marks a positional (`componentN`) entry; absence means the whole statement is positional.
    pub source_properties: HashMap<u32, Vec<Option<String>>>,
    /// Destructuring statements prepended for lambda parameters. Their component calls have no
    /// source line of their own in kotlinc debug information.
    pub lambda_parameters: HashSet<StmtId>,
    /// Loops whose variable is a destructuring pattern and whose body consumes the synthetic loop
    /// variable through a prepended destructuring statement.
    pub loops: HashSet<StmtId>,
    /// Square-bracket destructures parsed while `NameBasedDestructuring` is disabled. Retaining the
    /// opening bracket span lets the frontend report the language-version diagnostic after module
    /// admission without discarding declarations later in the file.
    pub ungated_bracket_spans: Vec<Span>,
    /// Explicit type annotations on destructured bindings, parallel to the statement's entries.
    pub entry_types: HashMap<u32, Vec<Option<TypeRef>>>,
}
