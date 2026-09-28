//! Declaration-stream identities of the callables a body declares: its local functions and the
//! accessors of its local delegated properties.

use super::*;
use crate::fir::BodyLocalCallableRole;

/// Bind a parser-local statement to its declaration-stream identity without retaining either the
/// parser id or a text coordinate. Parsing the same bounded declaration unit produces the same
/// local-function stream; the identity is discarded with the active checker after use.
pub(super) fn body_local_callable_declaration(
    file: &File,
    index: &ResolvedModuleIndex,
    owner: BodyOwnerId,
    statement: StmtId,
) -> Option<BodyLocalCallableDeclarationId> {
    declaration_in_stream(
        file,
        index,
        owner,
        statement,
        BodyLocalCallableRole::Function,
    )
}

/// The identity of the getter, or with `setter` the setter, kotlinc generates as a local function
/// for the local delegated property `statement` declares: counted in the same unit's stream of
/// local delegated properties, so it is as stable across two parses as a local function's.
pub(super) fn delegate_accessor_declaration(
    file: &File,
    index: &ResolvedModuleIndex,
    owner: BodyOwnerId,
    statement: StmtId,
    setter: bool,
) -> Option<BodyLocalCallableDeclarationId> {
    let role = if setter {
        BodyLocalCallableRole::DelegateSetter
    } else {
        BodyLocalCallableRole::DelegateGetter
    };
    declaration_in_stream(file, index, owner, statement, role)
}

fn declaration_in_stream(
    file: &File,
    index: &ResolvedModuleIndex,
    owner: BodyOwnerId,
    statement: StmtId,
    role: BodyLocalCallableRole,
) -> Option<BodyLocalCallableDeclarationId> {
    let mut declaration = DeclarationId::from_raw(owner.raw());
    while let Some(parent) = index
        .declaration_anchor(declaration)
        .and_then(|anchor| anchor.owner)
    {
        declaration = parent;
    }
    let owner = BodyOwnerId::from_raw(declaration.raw());
    let in_stream = |candidate: &Stmt| match role {
        BodyLocalCallableRole::Function => matches!(candidate, Stmt::LocalFun(_)),
        BodyLocalCallableRole::DelegateGetter | BodyLocalCallableRole::DelegateSetter => {
            matches!(candidate, Stmt::LocalDelegate { .. })
        }
    };
    let mut ordinal = 0u32;
    for (raw, candidate) in file.stmt_arena.iter().enumerate() {
        if !in_stream(candidate) {
            continue;
        }
        if raw == statement.0 as usize {
            return Some(BodyLocalCallableDeclarationId::with_role(
                owner, ordinal, role,
            ));
        }
        ordinal = ordinal.checked_add(1).expect("too many local declarations");
    }
    None
}
