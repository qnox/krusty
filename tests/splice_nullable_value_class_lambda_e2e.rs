//! A lambda over a nullable value class whose underlying type is a reference carries it unboxed
//! (`Tag?` as a nullable `String`). The byte splice still owns that shape, and coerces across the
//! `Object` of the `invoke` it replaces the way `StackValue.coerce` does: `unbox-impl` on entry and
//! `box-impl` on exit, each branching around null.

use super::common;

const LIB: &str = r#"
package lib

@JvmInline
value class Tag(val name: String)

inline fun retag(name: String, block: (Tag?) -> Tag?): Tag? = block(Tag(name))

inline fun untagged(block: (Tag?) -> Tag?): Tag? = block(null)
"#;

const MAIN: &str = r#"
import lib.*

fun renamed(name: String): String? = retag(name) { it?.let { inner -> Tag(inner.name + "!") } }?.name

fun dropped(name: String): String? = retag(name) { null }?.name

fun kept(): String? = untagged { it }?.name

fun box(): String {
    if (renamed("z") != "z!") return "FAIL renamed: " + renamed("z")
    if (dropped("y") != null) return "FAIL dropped: " + dropped("y")
    if (kept() != null) return "FAIL kept: " + kept()
    return "OK"
}
"#;

#[test]
fn nullable_reference_value_class_lambdas_run_like_the_reference_compiler() {
    let output = common::expect_box_run_against_kotlinc(LIB, MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}
