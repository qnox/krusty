// TARGET_BACKEND: JVM_IR
// `Array.clone()` is a JVM member, from `Cloneable`; other platforms' `Array` has none.
open class Reader(size: Int) {
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
