// A reference to a property declared in a local class's body, unbound (`A::p`) or as a delegate
// (`by A::p`), selects that property's accessors with its declared type applied to the receiver.

fun delegatedInExpressionBody(): String = run {
    class A(val tag: String) {
        val prop: String
            get() = tag
        val y by A::prop
    }
    A("a").y
}

fun box(): String {
    class B<T>(item: T) {
        var p: T = item
        val q: List<T> get() = listOf(p)
        val y by B<T>::q
        fun viaMember(): T = B<T>::p.get(this)
    }
    val b = B("b")
    B<String>::p.set(b, "c")
    val q = B<String>::q
    val result = delegatedInExpressionBody() + b.p + q.get(b)[0] + b.y[0] + b.viaMember() +
        B<String>::p.name
    return if (result == "accccp") "OK" else result
}
