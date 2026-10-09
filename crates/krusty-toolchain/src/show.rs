//! What `show` commands print, laid out as the toolchain lays it out.

mod dependencies;
mod modules;
mod settings;

pub use dependencies::module_dependencies;
pub use modules::{module_names, modules_table};
pub use settings::modules_settings;
