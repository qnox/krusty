// MODULE: lib
// FILE: text.kt
// Same-named extension properties declared in different files of one library module.
var stored = 0

val String.size: Int
    get() = length

var String.slot: Int
    get() = stored + length
    set(value) {
        stored = value
    }

fun textSize(text: String): Int = text.size

// FILE: number.kt
val Int.size: Int
    get() = this

var Int.slot: Int
    get() = stored * this
    set(value) {
        stored = value + this
    }

fun numberSize(number: Int): Int = number.size

// MODULE: main(lib)
// FILE: main.kt
fun box(): String {
    if ("ab".size != 2) return "fail text: ${"ab".size}"
    if (3.size != 3) return "fail number: ${3.size}"
    if (textSize("abc") + numberSize(4) != 7) return "fail library"
    "ab".slot = 5
    if ("ab".slot != 7) return "fail text slot: ${"ab".slot}"
    2.slot = 1
    if (2.slot != 6) return "fail number slot: ${2.slot}"
    return "OK"
}
