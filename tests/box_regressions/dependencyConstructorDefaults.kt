// MODULE: lib
// FILE: Plain.kt
class Plain(val raw: Int = 9, val label: String = "base")

// MODULE: box(lib)
// FILE: box.kt
fun box(): String {
    val all = Plain()
    if (all.raw != 9 || all.label != "base") return "all defaults"
    val partial = Plain(2)
    if (partial.raw != 2 || partial.label != "base") return "trailing default"
    val named = Plain(label = "x")
    if (named.raw != 9 || named.label != "x") return "leading default"
    return "OK"
}
