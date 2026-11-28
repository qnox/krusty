use super::common;

const LIBRARY: &str = r#"
package okio.internal

internal fun ByteArray.commonToUtf8String(beginIndex: Int = 0, endIndex: Int = size): String =
    String(this, beginIndex, endIndex - beginIndex)

internal fun String.commonAsUtf8ToByteArray(): ByteArray = toByteArray()
"#;

const MAIN: &str = r#"
@file:Suppress("INVISIBLE_MEMBER", "INVISIBLE_REFERENCE")

import okio.internal.commonAsUtf8ToByteArray
import okio.internal.commonToUtf8String

fun ByteArray.decode(): String = commonToUtf8String()

fun box(): String = "OK".commonAsUtf8ToByteArray().decode()
"#;

#[test]
fn a_suppressed_invisible_extension_of_another_module_is_selected() {
    let library = common::kotlinc_lib_out(&[("Lib.kt", LIBRARY)])
        .expect("reference kotlinc builds the internal extensions");
    assert_eq!(
        common::kotlinc_box_result_with_classpath(MAIN, std::slice::from_ref(&library)),
        "OK",
        "kotlinc's own result"
    );
    common::expect_box_ok_against("suppressed_invisible_extension", LIBRARY, MAIN);
}
