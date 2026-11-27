//! The contract between krusty and its compiler plugins.
//!
//! A plugin crate depends on this crate and nothing of the compiler: it sees only the items
//! exported here. The surface grows with a concrete plugin need, one narrow type at a time, and
//! never re-exports a compiler module.
//!
//! The first contract is the *convention plugin*: a kotlinc plugin whose whole effect is a list of
//! annotations the frontend then applies (all-open, no-arg, sam-with-receiver, assignment). A
//! convention plugin reads its `-P` options and answers with data; the frontend owns the matching.
//!
//! A plugin that transforms IR is a [`ModuleIrPlugin`]: it reads the module as [`ir::ModuleIr`] and
//! answers with the module it leaves; a symbol processor reads a round's declarations as [`ksp::KspRound`].

// Cargo otherwise accepts a sibling checkout's older source mtimes as proof that its typed model
// is unchanged. Track the workspace's existing identity in dep-info; standalone consumers need
// not define it, and repeated builds of the same checkout remain cached.
const _: Option<&str> = option_env!("KRUSTY_BUILD_CHECKOUT");

pub mod analysis;
mod annotation_names;
mod convention;
pub mod ir;
pub mod ksp;
mod module_ir_plugin;
mod option;

pub use annotation_names::{annotations_from_options, classifier_identities, Presets};
pub use convention::{Convention, ConventionPlugin};
pub use krusty_core::TypeName;
pub use module_ir_plugin::{ModuleIrPlugin, ModuleIrRefusal};
pub use option::PluginOption;
