// A member property whose type is the class's type parameter, read from another file of the
// module at a substituted type: the read produces the declared type and the use converts it.

// FILE: main.kt
fun box(): String {
    if (Holder<String>(null).value != null) return "Fail null"
    val text: String? = Holder("O").value
    return text + Holder("K").value
}

// FILE: declarations.kt
class Holder<T>(val value: T?)
