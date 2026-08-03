//! Kotlin permits newlines between a class header and its supertype-list colon. Keep the primary
//! constructor and colon on separate lines, matching declarations found in IntelliJ sources.

use super::common;

#[test]
fn class_header_colon_may_start_on_the_next_line() {
    const SRC: &str = r#"
open class Base(val value: String)

class Derived(value: String)
    : Base(value)

fun box(): String = Derived("OK").value
"#;

    common::expect_box_ok_with_stdlib(SRC, "ClassHeaderNewline");
}
