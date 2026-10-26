//! PLATFORM-SPECIFIC: JVM inline debug ranges end at their lexical initializer boundaries.

use super::common;

fn assert_artifacts_and_result(source: &str, stem: &str) {
    common::expect_box_same_as_kotlinc(source, stem);
    let classes = common::classes_against_kotlinc_module(&[("Use.kt", source)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn an_inline_initializer_keeps_its_marker_and_closing_scope() {
    let source = r#"
val a1 = "a".let {
    if (false) throw Error()
    it + "a"
}
fun box(): String = if (a1 == "aa") "OK" else "FAIL"
"#;
    assert_artifacts_and_result(source, "InlineInitializerScope");
}

#[test]
fn consecutive_inline_initializers_close_and_reuse_their_local_slots() {
    let source = r#"
val first = "O".let { val saved = it; saved }
val second = "K".let { val saved = it; saved }
fun box(): String = first + second
"#;
    assert_artifacts_and_result(source, "ConsecutiveInlineInitializerScopes");
}

#[test]
fn an_object_initializer_keeps_its_own_inline_scope() {
    let source = r#"
object Holder {
    val value = "O".let { val saved = it; saved + "K" }
}
fun box(): String = Holder.value
"#;
    assert_artifacts_and_result(source, "ObjectInlineInitializerScope");
}

#[test]
fn a_companion_initializer_keeps_its_inline_scope_before_an_init_block() {
    let source = r#"
var calls = 0
class Holder {
    companion object {
        val value = "O".let { val saved = it; saved + "K" }
        init { calls += 1 }
    }
}
fun box(): String {
    val result = Holder.value
    return if (calls == 1) result else "FAIL"
}
"#;
    assert_artifacts_and_result(source, "CompanionInlineInitializerScope");
}

#[test]
fn an_ordinary_callable_keeps_its_inline_local_ranges_through_the_return() {
    let source = r#"
fun make(value: String): String = value.let {
    if (false) throw Error()
    it + "K"
}
fun box(): String = make("O")
"#;
    assert_artifacts_and_result(source, "CallableInlineLocalScope");
}
