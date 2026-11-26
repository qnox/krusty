//! The synthetic `get<Name>$annotations` marker of a member property follows the property's own
//! access (`private`, `protected`; `internal` stays public) and, for an `internal` property, the
//! module suffix of its getter, as in kotlinc. Every emitted class is compared with kotlinc's.

use super::common;

const MARKERS: &str = include_str!("box_regressions/propertyAnnotationMarkerAccess.kt");

#[test]
fn member_property_markers_follow_the_property_access_and_module_suffix() {
    let classes = common::classes_against_kotlinc_module(&[("Main.kt", MARKERS)]);
    assert_eq!(classes.differences(), Vec::<String>::new(), "class files");
    common::expect_box_ok_files_with_stdlib(
        &[("Main.kt", MARKERS)],
        "property_annotation_marker_access",
    );
}
