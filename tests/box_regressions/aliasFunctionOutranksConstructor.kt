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

fun box(): String {
    val fn: String = Named(1)
    val ctor: Tag<String> = Named("x")
    val fn2: String = Same(1, 2)
    val ctor2: Tag<Int> = Same(3)
    val ctor3: Tag<Int> = Same(3, "y")
    return if (
        fn == "fn-named" && ctor.label == "ctor" && fn2 == "fn-same" &&
        ctor2.label == "ctor" && ctor3.label == "y"
    ) {
        "OK"
    } else {
        "FAIL $fn ${ctor.label} $fn2 ${ctor2.label} ${ctor3.label}"
    }
}
