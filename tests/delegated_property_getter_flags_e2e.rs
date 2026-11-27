//! An `interface by` property forwarder records its own getter as non-default when the forwarded
//! interface getter has a body, as kotlinc's `@Metadata` does; an abstract getter stays default.

use super::common;

const SOURCE: &str = include_str!("box_regressions/delegatedPropertyWithDefaultGetter.kt");

#[test]
fn delegated_property_forwarder_records_a_non_default_getter_when_the_forwarded_getter_has_a_body()
{
    common::assert_class_matches_kotlinc(
        "DelegatedPropertyWithDefaultGetter",
        SOURCE,
        "Delegating",
    );
    common::expect_box_same_as_kotlinc(SOURCE, "DelegatedPropertyWithDefaultGetterRun");
}

const MATCH_RESULT_SOURCE: &str = include_str!("box_regressions/delegatedMatchResult.kt");

#[test]
fn delegating_match_result_matches_kotlinc() {
    common::assert_class_matches_kotlinc("DelegatedMatchResult", MATCH_RESULT_SOURCE, "Matched");
    // A read-only collection `by` forwarder signs `containsAll` star-projected, as an override does.
    common::assert_class_matches_kotlinc("DelegatedMatchGroups", MATCH_RESULT_SOURCE, "Groups");
    common::expect_box_same_as_kotlinc(MATCH_RESULT_SOURCE, "DelegatedMatchResultRun");
}
