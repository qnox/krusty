//! Constructors reached by a bare `Name(args)` on the file and import scope tower, normalized into
//! the [`FunctionInfo`](crate::libraries::FunctionInfo) candidate structure same-named functions use.

use super::*;

impl Checker<'_> {
    /// The constructors of `internal` as receiver-less candidates of a bare call, from the same
    /// declaration set the constructor path materializes. A classifier that captures an outer
    /// instance contributes nothing here: its constructors are member candidates of the receiver
    /// supplying that instance.
    pub(super) fn scope_constructor_candidates(
        &self,
        internal: TypeName,
    ) -> Vec<crate::libraries::FunctionInfo> {
        let Some(classifier) = self.resolved_type_name(internal) else {
            return Vec::new();
        };
        if classifier.outer_instance.is_some() {
            return Vec::new();
        }
        self.constructor_declarations(internal, &classifier)
            .into_iter()
            .map(|declaration| {
                crate::libraries::FunctionInfo::scope_constructor(internal, &classifier, declaration)
            })
            .collect()
    }
}
