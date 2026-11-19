//! The collection members a target's runtime `ArrayList` reads another collection through.
//!
//! A target that lays `ArrayList` out itself (Kotlin/Wasm) runs `addAll`, `containsAll`,
//! `removeAll` and `retainAll` over an argument that may be any `Collection`: a list built from
//! an array (`ArrayAsCollection`), a set, or a program's own class. The stdlib walks the argument
//! with its `iterator()` and asks it `contains`, so that collection's own members run. The handoff
//! selects those members (`crate::resolve::collection_protocol::selected`) for a target that
//! realizes the list, which receives them frozen with each unit's other dependency declarations
//! (`crate::backend::CheckedBackendCallables::collection_protocol`).

use super::ExternalCallableId;

/// `Iterable.iterator()`, `Iterator.hasNext()`, `Iterator.next()` and `Collection.contains(E)`,
/// as their provider declares them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CollectionProtocol {
    pub iterator: ExternalCallableId,
    pub has_next: ExternalCallableId,
    pub next: ExternalCallableId,
    pub contains: ExternalCallableId,
}
