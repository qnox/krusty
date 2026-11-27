// MODULE: lib
// FILE: Outer.kt
class Outer(val name: String) {
    inner class Part(val tag: String = "d", val size: Int = 3) {
        fun describe() = name + ":" + tag + ":" + size
    }
}
// MODULE: box(lib)
// FILE: box.kt
fun box(): String {
    val outer = Outer("o")
    val all = outer.Part()
    if (all.describe() != "o:d:3") return "all defaults: " + all.describe()
    val trailing = outer.Part("x")
    if (trailing.describe() != "o:x:3") return "trailing default: " + trailing.describe()
    val leading = outer.Part(size = 7)
    if (leading.describe() != "o:d:7") return "leading default: " + leading.describe()
    return "OK"
}
