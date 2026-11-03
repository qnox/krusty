//! The JVM's reified-operation markers that its reified-operation pass records per expression,
//! while the semantic type-parameter identities are still in the IR, for emission to write.

use crate::fx_hash::HashMap;

use super::expression_copy::InheritedFacts;
use super::ExprId;

#[derive(Default)]
pub(crate) struct ReifiedMarkers {
    /// Try `ExprId` → source spelling of each catch that is still a reified parameter of a
    /// declaration in this file, parallel to that try's catches. `None` is an ordinary clause.
    /// Try emission reads it for `reifiedOperationMarker` mode 7 and does not recover the
    /// spelling from the identity.
    pub(crate) catches: HashMap<ExprId, Vec<Option<String>>>,
    /// `NewArray`/`Vararg` `ExprId` → the `NEW_ARRAY` marker argument of an array a reified
    /// declaration creates of its own parameter, kotlinc's `ReificationArgument.asString`: a `[`
    /// per nested array level, the parameter's source spelling, and `?` for a marked-nullable
    /// element. Array emission writes it, mode 0, before the `anewarray`.
    pub(crate) arrays: HashMap<ExprId, String>,
}

impl InheritedFacts for ReifiedMarkers {
    fn inherit(&mut self, source: ExprId, target: ExprId) {
        let Self { catches, arrays } = self;
        catches.inherit(source, target);
        arrays.inherit(source, target);
    }
}
