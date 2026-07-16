//! Signature-phase inference for top-level property initializers that read earlier properties,
//! including nested calls and imported extension properties.
mod common;
use common::assert_box_ok_with_stdlib;

#[test]
fn toplevel_property_cross_reference() {
    let src = r#"
const val a = '1'
const val b = '2'
const val cmp = a.compareTo(b)
const val code = a.code
const val alias = code
fun box(): String {
    if (cmp >= 0) return "fail: cmp"
    if (code != 49) return "fail: code"
    if (alias != 49) return "fail: alias"
    return "OK"
}
"#;
    assert_box_ok_with_stdlib(src, "toplevel_property_cross_reference");
}
