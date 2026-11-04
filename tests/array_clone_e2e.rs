//! `Array<T>.clone()` returns the receiver's own application: `Array<String?>.clone()` is an
//! `Array<String?>`, as moshi's `JsonReader` copy constructor relies on.

use super::common;

const NULLABLE_ELEMENTS: &str = r#"open class Reader(size: Int) {
    protected var names: Array<String?> = arrayOfNulls(size)
    protected var depths: Array<Int?> = arrayOfNulls(size)

    protected constructor(copyFrom: Reader) : this(0) {
        names = copyFrom.names.clone()
        depths = copyFrom.depths.clone()
    }

    fun set(index: Int, name: String?) { names[index] = name; depths[index] = name?.length }
    fun render(): String = names.joinToString(",") + "/" + depths.joinToString(",")
}

class Copy(source: Reader) : Reader(source)

fun <T> cloned(values: Array<T>): Array<T> = values.clone()

fun box(): String {
    val reader = Reader(2)
    reader.set(1, "id")
    val copy = Copy(reader)
    reader.set(0, "late")
    val strings: Array<String?> = cloned(arrayOf("a", null))
    val text = copy.render() + "|" + strings.joinToString(",")
    return if (text == "null,id/null,2|a,null") "OK" else text
}
"#;

#[test]
fn array_clone_keeps_nullable_element_types() {
    common::expect_box_same_as_kotlinc(NULLABLE_ELEMENTS, "ArrayClone");
}
