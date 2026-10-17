//! An integer literal is an input occurrence of a nullable `@OnlyInputTypes` solution.
//!
//! `ids.contains(2001)` with `ids: Iterable<Long?>` solves `T := Long?` from the receiver, and the
//! literal becomes a `Long` there, as `val id: Long? = 2001` does. The candidate check compared
//! the literal only against a non-null `Long`, so every candidate failed.

use super::common;

const SOURCE: &str = r#"
class Job(var id: Long? = null)

fun box(): String {
    val ids: Iterable<Long?> = listOf(2001L, null)
    if (!ids.contains(2001)) return "contains"
    if (ids.indexOf(2001) != 0) return "indexOf"
    val job = Job(2001)
    if (!listOf(job.id).asIterable().contains(2001)) return "job"
    return "OK"
}
"#;

#[test]
fn an_integer_literal_witnesses_a_nullable_only_input_type() {
    common::expect_box_same_as_kotlinc(SOURCE, "OnlyInputIntegerLiteral");
    let classes = common::classes_against_kotlinc_module(&[("Main.kt", SOURCE)]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    for (class, reference) in &classes.reference {
        assert!(
            classes.krusty[class] == *reference,
            "{class} differs from kotlinc"
        );
    }
}
