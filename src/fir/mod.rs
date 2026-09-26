//! Streaming frontend ownership model.
//!
//! The module tree follows frontend lifetime boundaries: stable header inventory, temporary
//! signature solving, checked body ownership, and exhaustive parser coverage.

mod active_source;
mod body;
mod body_check;
mod body_work;
mod capture;
pub mod coverage;
mod declaration_stub;
mod declared_supertypes;
mod delegate_calls;
mod header;
mod identities;
mod inline_body;
mod local_callables;
mod local_class_capture;
mod local_class_names;
mod lookup_scope;
mod module_symbols;
mod overrides;
mod parameters;
mod signature;
mod signature_extract;
mod source_map;
mod type_parameters;

pub(crate) use active_source::*;
pub use body::*;
pub use body_check::*;
pub use body_work::*;
pub use capture::*;
pub use declared_supertypes::{superclass_slot, DeclaredSuperclass};
pub use delegate_calls::*;
pub use header::*;
pub use inline_body::*;
pub use local_callables::BodyLocalCallableDeclarationId;
pub use local_class_capture::*;
pub use local_class_names::*;
pub(crate) use module_symbols::*;
pub use overrides::*;
pub use parameters::*;
pub use signature::*;
pub use signature_extract::*;
pub use type_parameters::*;

#[cfg(test)]
mod body_check_tests;
#[cfg(test)]
mod index_tests;
#[cfg(test)]
mod signature_tests;
#[cfg(test)]
mod tests;
