//! Checked array factories retain the flat annotation values selected by their spread operands.
// PLATFORM: compare complete JVM class inventories and raw class artifacts against kotlinc.

use super::common;

const SOURCE: &str = r#"enum class Choice { FIRST, SECOND }
annotation class Texts(val value: Array<String>)
annotation class Numbers(val value: IntArray)
annotation class Choices(val value: Array<Choice>)

@Texts(arrayOf(*arrayOf("O"), "K", *arrayOf(elements = arrayOf("!"))))
@Numbers(intArrayOf(1, *intArrayOf(2, *intArrayOf(3))))
@Choices(arrayOf(*arrayOf(*emptyArray()), Choice.FIRST, *arrayOf(elements = arrayOf(*[Choice.SECOND]))))
fun target(): String = "OK"

@Texts(arrayOf(elements = emptyArray()))
@Numbers(intArrayOf())
@Choices(arrayOf(Choice.SECOND))
fun emptyAndPlain(): String = "OK"

fun box(): String = if (target() == "OK" && emptyAndPlain() == "OK") "OK" else "bad"
"#;

#[test]
fn spread_and_named_array_operands_match_complete_artifacts() {
    let classes = common::classes_against_kotlinc_module(&[("AnnotationArraySpreads.kt", SOURCE)]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>(),
        "complete class inventory"
    );
    let differences = classes
        .reference
        .iter()
        .filter_map(|(name, reference)| {
            let ours = &classes.krusty[name];
            (reference != ours).then(|| common::exact_class_difference(name, reference, ours))
        })
        .collect::<Vec<_>>();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

#[test]
fn spread_and_named_array_operands_run_like_kotlin() {
    common::expect_box_same_as_kotlinc(SOURCE, "AnnotationArraySpreads");
}
