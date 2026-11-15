// A same-named function takes a bare call written through a type alias when it applies, and an
// inapplicable one leaves the call to the aliased class's constructors, whose arguments the alias
// fixes (kotlinc 2.4.21).
class Tag<T>(val value: T, val label: String) {
    constructor(value: T) : this(value, "ctor")
}

typealias Named = Tag<String>
typealias Same<T> = Tag<T>

fun Named(a: Int): String = "fn-named"

fun Same(a: Int, b: Int): String = "fn-same"

typealias Boxed = Tag<String>

// The function's own type parameter is inferred from the call, not constrained by the alias.
fun <T> Boxed(a: Int, b: Int, c: Int, f: () -> T): T = f()

typealias Broad = Tag<String>
fun Broad(value: Any): String = "fn-broad"
fun inspectAliasResult(value: Any): String = if (value is String) value else "constructor"

class PairTag<A, B>(val first: A, val second: B)
typealias Projected<T> = PairTag<String, T>
fun Projected(): String = "fn-projected"

fun box(): String {
    val fn: String = Named(1)
    val ctor: Tag<String> = Named("x")
    val fn2: String = Same(1, 2)
    val ctor2: Tag<Int> = Same(3)
    val ctor3: Tag<Int> = Same(3, "y")
    val inferred: Int = Boxed(1, 2, 3) { 7 }
    val broad = inspectAliasResult(Broad(1))
    val projected = Projected<Int>("fixed", 9)
    return if (
        inferred == 7 && broad == "fn-broad" &&
        projected.first == "fixed" && projected.second == 9 &&
        fn == "fn-named" && ctor.label == "ctor" && fn2 == "fn-same" &&
        ctor2.label == "ctor" && ctor3.label == "y"
    ) {
        "OK"
    } else {
        "FAIL $fn ${ctor.label} $fn2 ${ctor2.label} ${ctor3.label}"
    }
}
