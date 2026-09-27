//! Kotlin's program entry-point rule over one finalized top-level function header, and the entry
//! the frontend selected with it for each source unit.
//!
//! The resolver applies the rule once, after signatures are final: it keeps an entry point
//! file-local when it classifies top-level overload conflicts, and it records the entry it selects
//! for each source unit in the module index. Common lowering maps that recorded identity; it never
//! applies the rule again. The rule genuinely names a declaration (`main`), so it is stated here.

use super::{CallableId, ResolvedModuleIndex, SourceFileId};
use crate::types::Ty;

/// The parameter form of a Kotlin `main` entry point.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MainEntryParameters {
    /// `fun main()`.
    None,
    /// `fun main(args: Array<String>)`, including `vararg args: String`.
    Arguments,
}

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
    /// parameters, return `Unit`, and take either nothing or one array of `String`.
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
            [parameter] if parameter.array_read_elem() == Some(Ty::String) => {
                Some(MainEntryParameters::Arguments)
            }
            _ => None,
        }
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
}
