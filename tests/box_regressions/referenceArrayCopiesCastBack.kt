// A reference array's copy is built as an `Array<Any?>` and cast back to the receiver's
// `Array<T>`; the copy and an explicit array cast both keep the array's own object.
fun <T> Array<T>.twice(): Array<T> = copyOf(size * 2).let { it as Array<T> }

fun box(): String {
    val strings = arrayOf("a", "b", "c")
    val copy = strings.copyOf()
    if (copy.size != 3 || copy[0] != "a" || copy[2] != "c") return "copyOf: ${copy.joinToString()}"
    copy[0] = "z"
    if (strings[0] != "a") return "copy shares storage"
    val range = strings.copyOfRange(1, 3)
    if (range.size != 2 || range[0] != "b") return "copyOfRange: ${range.joinToString()}"
    val doubled = strings.twice()
    if (doubled.size != 6 || doubled[2] != "c" || doubled[3] != null) return "twice: ${doubled.joinToString()}"
    val any: Any = strings
    val back = any as Array<String>
    if (back[1] != "b") return "cast: ${back.joinToString()}"
    val nullable: Any? = null
    if ((nullable as Array<String>?) != null) return "nullable cast"
    return "OK"
}
