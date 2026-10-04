//! A type-parameter bound resolves its classifier in the declaration's lexical scope. A bound
//! that names a nested classifier keeps it in the `Signature` attribute and the metadata,
//! instead of erasing to `Object` through the module-wide spelling table.

use super::common;

const ACCEPTED: &str = r#"
open class Own
class Outer {
    open class Own
    fun <T : Own> picksNested(t: T): T = t
    class Deeper {
        fun <T : Own> picksOuter(t: T): T = t
        open class Own
        fun <T : Own> picksDeeper(t: T): T = t
    }
    class Gen<T : Own>(val t: T)
    companion object {
        open class Tag
    }
    fun <T : Tag> companionNested(t: T): T = t
    val <T : Own> T.prop: T get() = this
    interface Api { fun <T : Own> api(t: T): T }
}
fun <T : Own> topLevel(t: T): T = t

fun box(): String {
    val outer = Outer()
    val nested = Outer.Own()
    if (outer.picksNested(nested) !== nested) return "nested"
    if (Outer.Gen(nested).t !== nested) return "gen"
    val tag = Outer.Companion.Tag()
    if (outer.companionNested(tag) !== tag) return "companion"
    val deeper = Outer.Deeper.Own()
    if (Outer.Deeper().picksDeeper(deeper) !== deeper) return "deeper"
    val top = Own()
    if (topLevel(top) !== top) return "top"
    return "OK"
}
"#;

#[test]
fn nested_classifier_bounds_are_signed_like_kotlinc() {
    common::assert_accepted_like_kotlinc(ACCEPTED);
    assert_eq!(
        common::expect_box_run_with_stdlib(ACCEPTED, "NestedBounds"),
        "OK"
    );
    common::assert_classes_identical_to_kotlinc(
        "NestedBounds",
        ACCEPTED,
        &[
            "Outer",
            "Outer$Deeper",
            "Outer$Gen",
            "Outer$Api",
            "NestedBoundsKt",
        ],
    );
}
