//! PLATFORM: JVM A companion property's initializer opens its class-initializer store on the
//! initializer's own line and keeps the lines it marks inside itself, as kotlinc's
//! `InitializersLowering` positions the field store at the initializer expression.

use super::common;

const SOURCE: &str =
    include_str!("fixtures/companion_static_initializer_line/CompanionStaticInitializerLine.kt");

#[test]
fn companion_static_initializer_lines_match_every_class_artifact() {
    let classes =
        common::classes_against_kotlinc_module(&[("CompanionStaticInitializerLine.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn companion_static_initializers_keep_their_values() {
    common::expect_box_same_as_kotlinc(SOURCE, "CompanionStaticInitializerLine");
}
