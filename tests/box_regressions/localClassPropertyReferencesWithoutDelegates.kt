// A reference to a property declared in a local class's body (`A::p`) selects that property's
// accessors, with its declared type applied to the receiver's type arguments.

class Two<U>(val first: U, val second: U)

fun fromExpressionBody(): String = run {
    class A(val tag: String) {
        val prop: String
            get() = tag
        fun viaMember(): String = A::prop.get(this)
    }
    A("a").viaMember()
}

fun box(): String {
    class B<T>(item: T) {
        var p: T = item
        val q: Two<T> get() = Two(p, p)
        fun viaMember(): T = B<T>::p.get(this)
    }
    val b = B("b")
    B<String>::p.set(b, "c")
    val q = B<String>::q
    // Each value is compared where it is read: a generic member's value is not rendered into a
    // string, which not every backend can do for a local class's instances.
    if (fromExpressionBody() != "a") return "FAIL expression body"
    if (b.p != "c") return "FAIL set"
    if (q.get(b).first != "c") return "FAIL composite"
    if (b.viaMember() != "c") return "FAIL member"
    if (B<String>::p.name != "p") return "FAIL name"
    return "OK"
}
