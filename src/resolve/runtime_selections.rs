//! The dependency declarations a lowering after checking calls whether or not the source names
//! them. Such a lowering cannot select a declaration, so resolution selects each once per
//! compilation and the handoff freezes the selection with each unit's other dependency
//! declarations.

use crate::symbol_source::SymbolSource;

/// What resolution selected for the lowerings that run after checking.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RuntimeSelections {
    /// The declarations suspend lowering calls.
    pub(crate) coroutine_runtime: Option<crate::ir::IrCoroutineRuntime>,
    /// The `CharSequence` members a builder that reads an appended sequence through them calls.
    pub(crate) char_sequence_protocol: Option<crate::fir::CharSequenceProtocol>,
    /// The `enumEntries(Array)` an enum realization building `entries` the way kotlinc-js/wasm do
    /// calls.
    pub(crate) enum_entries_factory: Option<crate::fir::EnumEntriesFactory>,
}

impl RuntimeSelections {
    pub(super) fn select(source: &dyn SymbolSource) -> Self {
        Self {
            coroutine_runtime: super::coroutine_runtime::selected(source),
            char_sequence_protocol: super::char_sequence_protocol::selected(source),
            enum_entries_factory: super::enum_entries_factory::selected(source),
        }
    }
}
