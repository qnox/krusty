//! PLATFORM: official-producer top-level properties and Native dependency execution.

use super::{assert_official_dependency_closures, DependencyConsumer};

#[test]
fn dependency_top_level_properties_share_one_storage_location() {
    assert_official_dependency_closures(
        include_str!("../fixtures/klib_closure_frames/TopLevelProperties.kt"),
        include_str!("../fixtures/klib_closure_frames/TopLevelPropertyConsumer.kt"),
        "DependencyTopLevelProperties",
        DependencyConsumer::Native,
    );
}
