//! A getter's expression body returns at the expression's end line, as kotlinc marks it: fir2ir
//! builds an accessor's implicit return as it builds a function's, so a getter spanning several
//! lines returns on its last one.

use super::common;

const SOURCE: &str = r#"class Box(val v: Int) {
    fun grow(n: Int): Box = Box(v + n)

    val doubled: Int
        get() = v +
            v

    val oneLine get() = v * 3

    val block: Int
        get() {
            return v +
                1
        }
}

val Box.plainGrown
    get() = grow(
        2)

val Box?.grown
    get() = this?.grow(
        2)

val Box.trimmed
    get() = "  $v ".trim(
    )

var Box.label: String
    get() = "b" +
        v
    set(value) = println(
        value)

fun box(): String {
    val box = Box(2)
    box.label = "x"
    val total = box.doubled + box.oneLine + box.block + box.plainGrown.v + box.grown!!.v + box.trimmed.length
    if (box.label != "b2") return box.label
    return if (total == 4 + 6 + 3 + 4 + 4 + 1) "OK" else "total $total"
}
"#;

#[test]
fn a_multi_line_getter_body_returns_on_its_last_line_like_kotlinc() {
    common::expect_box_ok_with_stdlib(SOURCE, "GetterReturnLine");
    common::assert_classes_identical_to_kotlinc(
        "GetterReturnLine",
        SOURCE,
        &["Box", "GetterReturnLineKt"],
    );
}
