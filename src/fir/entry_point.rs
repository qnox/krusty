//! Kotlin's program entry-point rule over one finalized top-level function header, and the entry
//! the frontend selected with it for each source unit.
//!
//! The resolver applies the rule once, after signatures are final: it keeps an entry point
//! file-local when it classifies top-level overload conflicts, and it records the entry it selects
//! for each source unit in the module index. Common lowering maps that recorded identity; it never
//! applies the rule again. The rule genuinely names a declaration (`main`), so it is stated here.
//!
//! The `codegen/box` convention is selected the same way: a test program starts in its
//! `fun box(): String`, which a runnable backend calls and whose answer it prints. Kotlin itself
//! gives `box` no meaning, so it is not an entry point for conflicts; it is only recorded.

use super::{CallableId, ResolvedModuleIndex, SourceFileId};
use crate::program_entry::MainEntryParameters;
use crate::types::Ty;

/// The finalized header facts the entry-point rule reads from one top-level function.
#[derive(Clone, Copy, Debug)]
pub struct MainEntryShape<'a> {
    pub name: &'a str,
    pub has_extension_receiver: bool,
    pub type_parameter_count: usize,
    pub context_parameter_count: usize,
    pub parameters: &'a [Ty],
    pub result: Ty,
}

impl MainEntryShape<'_> {
    /// The entry-point form of this function, or `None` when Kotlin does not treat it as `main`:
    /// it must be named `main`, have no extension receiver, type parameters, or context
    /// parameters, return `Unit`, and take either nothing or one array whose elements read as a
    /// non-null `String` (`Array<String>`, `Array<out String>`, `vararg String`). The array itself
    /// may be nullable; `Array<String?>`, `Array<in String>` and `Array<*>` are not entry points.
    pub fn entry_parameters(&self) -> Option<MainEntryParameters> {
        if self.name != "main"
            || self.has_extension_receiver
            || self.type_parameter_count != 0
            || self.context_parameter_count != 0
            || self.result != Ty::Unit
        {
            return None;
        }
        match self.parameters {
            [] => Some(MainEntryParameters::None),
            [parameter] if parameter.non_null().array_read_elem() == Some(Ty::String) => {
                Some(MainEntryParameters::Arguments)
            }
            _ => None,
        }
    }

    /// Whether this is a `codegen/box` test entry: a `box` with no extension receiver, type
    /// parameters, context parameters or parameters whose result the checker's subtyping
    /// (`is_string`, asked of a type outside the `Nothing` family) places under `String?`. The
    /// corpus declares both `box(): String` and `box(): String?`.
    pub fn is_box_entry(&self, is_string: impl FnOnce(Ty) -> bool) -> bool {
        self.name == "box"
            && !self.has_extension_receiver
            && self.type_parameter_count == 0
            && self.context_parameter_count == 0
            && self.parameters.is_empty()
            && self.result.non_null().canonical_semantic() != Ty::Nothing
            && is_string(self.result)
    }
}

/// The Kotlin `main` the frontend selected for one source unit. When the unit declares both
/// forms, `main(args: Array<String>)` is the entry and the parameterless `main()` is an ordinary
/// function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedEntryPoint {
    pub callable: CallableId,
    pub parameters: MainEntryParameters,
}

impl ResolvedModuleIndex {
    /// The entry point the frontend selected for `source`, if it declares one.
    pub fn source_entry_point(&self, source: SourceFileId) -> Option<ResolvedEntryPoint> {
        self.source_entry_points.get(&source).copied()
    }

    pub(crate) fn publish_source_entry_point(
        &mut self,
        source: SourceFileId,
        entry: ResolvedEntryPoint,
    ) {
        assert!(
            self.source_entry_points.insert(source, entry).is_none(),
            "a source unit selects one entry point"
        );
    }

    /// The `fun box(): String` the frontend selected for `source`, if it declares exactly one.
    pub fn source_box_entry(&self, source: SourceFileId) -> Option<CallableId> {
        self.source_box_entries.get(&source).copied()
    }

    pub(crate) fn publish_source_box_entry(&mut self, source: SourceFileId, callable: CallableId) {
        assert!(
            self.source_box_entries.insert(source, callable).is_none(),
            "a source unit selects one box entry"
        );
    }
}
