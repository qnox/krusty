//! A property assignment realized as a field store marks the assignment's line at the
//! `putfield`, as kotlinc's `visitSetField` does: an entry is written when the value ran on
//! another line, or under an inlined body after which the line in effect is forgotten.

use super::common;

const SOURCE: &str = r#"
inline fun passed(a: Any?): Any? = a

class Holder {
    var value: Any? = null
    fun onNextLine(w: Any?) {
        value =
            w
    }
    fun inlined(w: Any?) {
        value = passed(w)
    }
}
"#;

#[test]
fn a_field_store_marks_the_assignments_line_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc("FieldStoreLines", SOURCE, &["Holder"]);
}
