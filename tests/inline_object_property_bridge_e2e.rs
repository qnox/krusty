//! An inline function's anonymous object overrides an interface property with a narrower type, the
//! type of another anonymous object. Each call site copies the outer object, and the copy's getter
//! keeps the narrower result, so the copy needs the interface's getter bridge as the declaration
//! does.
//!
//! kotlinc 2.4 writes classes for this program that throw `AbstractMethodError` (KT-20245), so it
//! cannot be the oracle of a shared box regression on every release. The reference comparison is
//! the 2.5 corpus case `inline/kt20245.kt`. This pins krusty's own run.

use super::common;

const PROGRAM: &str = r#"
interface HasValue {
    val value: Any
}

inline fun wrap(crossinline text: () -> String) =
    object : HasValue {
        override val value = object {
            override fun toString() = text()
        }
    }

fun read(holder: HasValue): String = holder.value.toString()

fun box(): String = read(wrap { "O" }) + read(wrap { "K" })
"#;

#[test]
fn an_inline_object_copy_bridges_its_narrowed_property_getter() {
    assert_eq!(
        common::expect_box_run_with_stdlib(PROGRAM, "InlineObjectPropertyBridge"),
        "OK"
    );
}
