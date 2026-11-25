//! The stdlib function an enum's `entries` list is built by on a target that builds it the way
//! kotlinc-js/wasm do: `kotlin.enums.enumEntries(entries: Array<E>)`, whose library body wraps the
//! array in the stdlib's own `EnumEntriesList`.
//!
//! The shared enum realization (`crate::backend::enums`) calls it after checking, where no lookup
//! may run, so resolution selects it once per compilation
//! (`crate::resolve::PassTwoSymbols::enum_entries_factory`). The realization writes that identity
//! into the call it builds, and the dependency prewalk reads the same call from the emitted bodies,
//! so both name one selected declaration.

use super::ExternalCallableId;

/// `kotlin.enums.enumEntries<E>(entries: Array<E>): EnumEntries<E>`, as its provider declares it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnumEntriesFactory {
    /// The selected declaration. Its one type parameter, ordinal 0, is the enum.
    pub function: ExternalCallableId,
}
