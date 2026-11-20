//! Two lower bounds of a type variable join at their common supertype whatever their kind, as
//! kotlinc's do: `Int` and `String` meet at `Comparable<*> & Serializable`, and `Int` and `Double`
//! below `T : Number` at `Number & Comparable<*>`, which the variable's bound approximates to
//! `Number` where a declaration publishes it.
use super::common;

#[test]
fn mixed_lower_bounds_join_at_their_common_supertype() {
    let source = "\
fun <T> any(vararg values: T): T = values[0]
fun <T : Number> first(vararg values: T): T = values[0]
fun number() = first(1, 4.5)
fun check() {
    val joined = any(1, \"a\")
    val a: Int = joined
    val b: Int = number()
    val d: Int = any(1, 2L)
}
";
    let sources = [("test.kt", source)];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
    assert_eq!(
        common::reference_error_blocks(&sources, &[]),
        [
            "test.kt:6:16: initializer type mismatch: expected 'Int', actual 'Comparable<*> & \
             Serializable'.",
            "test.kt:7:16: initializer type mismatch: expected 'Int', actual 'Number'.",
            "test.kt:8:16: initializer type mismatch: expected 'Int', actual 'Long'.",
        ]
    );
}

#[test]
fn a_mixed_join_runs_through_its_components() {
    let source = include_str!("box_regressions/MixedLowerBoundJoin.kt");
    common::expect_box_same_as_kotlinc(source, "mixed_lower_bound_join");
}
