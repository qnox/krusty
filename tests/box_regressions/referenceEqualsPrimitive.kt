class Holder(val value: Any?)

var reads = ""

fun read(holder: Holder, tag: String): Any? {
    reads += tag
    return holder.value
}

fun box(): String {
    val int: Any = 3
    if (int != 3) return "int"
    if (int == 4) return "other int"
    if (int == 3L) return "int as long"
    if (3 != int) return "int on the left"

    val long: Any = 5L
    if (long != 5L || long == 5) return "long"

    val char: Any = 'c'
    if (char != 'c' || char == 'd') return "char"

    val flag: Any = true
    if (flag != true || flag == false) return "boolean"

    val text: Any = "3"
    if (text == 3) return "string against int"

    val nullable: Int? = 7
    if (nullable != 7 || nullable == 8) return "nullable int"
    val absent: Int? = null
    if (absent == 7) return "null against int"

    val cells = arrayOf<Any?>(1, null, 'x')
    if (cells[0] != 1 || cells[1] == 1 || cells[2] != 'x') return "array elements"

    if (read(Holder(9), "l") != 9 || reads != "l") return "evaluated once: $reads"
    return "OK"
}
