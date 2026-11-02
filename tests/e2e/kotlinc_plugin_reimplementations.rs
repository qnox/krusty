//! kotlinc compiler plugins krusty reimplements natively, each compared byte for byte with the
//! reference compiler through the shared plugin fixture.

pub(super) use super::{common, compiler_plugin_fixture};

#[path = "../allopen_plugin_e2e.rs"]
mod allopen_plugin_e2e;
#[path = "../assignment_plugin_e2e.rs"]
mod assignment_plugin_e2e;
#[path = "../noarg_plugin_e2e.rs"]
mod noarg_plugin_e2e;
#[path = "../parcelize_plugin_e2e.rs"]
mod parcelize_plugin_e2e;
#[path = "../sam_with_receiver_plugin_e2e.rs"]
mod sam_with_receiver_plugin_e2e;
