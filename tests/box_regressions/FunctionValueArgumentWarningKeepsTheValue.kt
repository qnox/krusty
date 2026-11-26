// An argument that only draws a warning does not make a function value inapplicable: kotlinc
// still calls the nearer value, not the implicit receiver's member of the same name.

@Deprecated("kept for the warning")
fun legacy(): Int = 1

class Box {
    fun f(x: Int): String = "member$x"
}

fun Box.declared(f: (Int) -> String): String = f(legacy())

fun Box.inferred(f: (Int) -> String) = f(legacy())

fun box(): String {
    val declared = Box().declared { "value$it" }
    if (declared != "value1") return "declared: $declared"
    val inferred = Box().inferred { "value$it" }
    if (inferred != "value1") return "inferred: $inferred"
    return "OK"
}
