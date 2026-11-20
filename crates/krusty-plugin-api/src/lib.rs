//! The contract between krusty and its native compiler plugins.
//!
//! A plugin crate depends on this crate and nothing of the compiler: it sees only the items
//! exported here. The surface grows with a concrete plugin need, one narrow type at a time, and
//! never re-exports a compiler module.
//!
//! The first contract is the *convention plugin*: a kotlinc plugin whose whole effect is a list of
//! annotations the frontend then applies (all-open, no-arg, sam-with-receiver, assignment). A
//! convention plugin reads its `-P` options and answers with data; the frontend owns the matching.

mod annotation_names;
mod convention;
mod option;

pub use annotation_names::{annotations_from_options, classifier_identities, Presets};
pub use convention::{Convention, ConventionPlugin};
pub use krusty_names::TypeName;
pub use option::PluginOption;
