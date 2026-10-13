//! A top-level property's JVM annotation carrier is a real facade method for collision purposes,
//! even though the method is synthesized after common lowering.

use super::common;

#[test]
fn a_property_annotation_marker_clashes_with_a_source_function() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "MarkerClash.kt",
            "package store\n\
             \n\
             @Suppress(\"VOLATILE_ON_VALUE\")\n\
             @Volatile val value: Int = 1\n\
             fun `getValue$annotations`() {}\n",
        )],
        &[],
    );
}
