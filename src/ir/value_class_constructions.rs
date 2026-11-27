//! The value class an erased construction expression belongs to.

use super::{ExprId, IrFile};
use crate::types::TypeName;

impl IrFile {
    /// The value class a construction expression erased to its underlying value, which is the
    /// class the expression's unboxed result belongs to whatever type its `constructor-impl`
    /// function declares (`Level(val id: Id)` constructs a `Level` through a function returning
    /// `Id`).
    pub fn erased_value_construction(&self, expression: ExprId) -> Option<TypeName> {
        self.erased_value_constructions
            .get(&expression)
            .map(|(owner, _)| *owner)
    }
}
