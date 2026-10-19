//! The JVM's reified-operation markers that its reified-operation pass records per expression,
//! while the semantic type-parameter identities are still in the IR, for emission to write.

use crate::fx_hash::HashMap;

use super::{ExprId, IrFile};

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

impl IrFile {
    /// Give the copy `target` of expression `source` the markers recorded for `source`.
    pub(super) fn copy_reified_markers(&mut self, source: ExprId, target: ExprId) {
        let markers = &mut self.reified_markers;
        if let Some(catches) = markers.catches.get(&source).cloned() {
            markers.catches.insert(target, catches);
        }
        if let Some(array) = markers.arrays.get(&source).cloned() {
            markers.arrays.insert(target, array);
        }
    }
}
