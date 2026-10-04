//! Streaming frontend ownership model.
//!
//! The module tree follows frontend lifetime boundaries: stable header inventory, temporary
//! signature solving, checked body ownership, and exhaustive parser coverage.

mod active_source;
mod annotation_constructor_defaults;
mod body;
mod body_check;
mod body_work;
mod capture;
pub mod coverage;
mod declaration_stub;
mod delegate_calls;
mod entry_point;
mod header;
mod identities;
mod inline_body;
mod local_callables;
mod local_class_capture;
mod local_class_names;
mod local_delegated_properties;
mod lookup_scope;
mod module_symbols;
mod overrides;
mod parameters;
mod retained_bodies;
mod signature;
mod signature_extract;
mod signature_source;
mod source_coordinates;
mod source_lambda;
mod source_map;
mod type_parameters;

pub(crate) use active_source::*;
pub(crate) use annotation_constructor_defaults::publish_checked_annotation_defaults;
pub use body::value_parameters::FirValueParameterName;
pub use body::*;
pub use body_check::*;
pub use body_work::*;
pub use capture::*;
pub use delegate_calls::*;
pub use entry_point::{MainEntryShape, ResolvedEntryPoint};
pub use header::*;
pub use inline_body::*;
pub use local_callables::BodyLocalCallableDeclarationId;
pub use local_class_capture::*;
pub use local_class_names::*;
pub use local_delegated_properties::LocalDelegatedPropertyId;
pub(crate) use local_delegated_properties::{
    FirLocalDelegateDispatchParameter, FirLocalDelegatePlan, LocalDelegateBinding,
};
pub(crate) use module_symbols::*;
pub use overrides::*;
pub use parameters::*;
pub use retained_bodies::*;
pub use signature::*;
pub use signature_extract::*;
pub use source_lambda::*;
pub use type_parameters::*;

#[cfg(test)]
mod body_check_tests;
#[cfg(test)]
mod entry_point_tests;
#[cfg(test)]
mod index_tests;
#[cfg(test)]
mod signature_tests;
#[cfg(test)]
mod tests;
