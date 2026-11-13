// A context parameter of a qualified inner type: resolving `Outer.Inner` must not consult the
// declaration's own context parameters for the outer's type arguments.
class Outer(val name: String) {
    inner class Inner {
        fun read() = name
    }
}

context(inner: Outer.Inner)
fun readInner(): String = inner.read()

fun box(): String = with(Outer("OK").Inner()) { readInner() }
