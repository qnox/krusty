//! kotlinc's `FirValueClassDeclarationChecker`, one child module per loop of it that krusty runs:
//! the primary-constructor parameters and the reserved member names.

mod parameters;
mod reserved_members;

use super::*;

impl Checker<'_> {
    /// Check the value class declaration `cl`, whose classifier is `owner`.
    pub(super) fn check_value_class_declaration(
        &mut self,
        scope: &CheckerScope<'_>,
        cl: &ClassDecl,
        owner: Option<TypeName>,
    ) {
        self.check_value_class_parameters(scope, cl, owner);
        self.check_value_class_reserved_members(scope, cl, owner);
    }
}
