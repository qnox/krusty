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
    val result = fromExpressionBody() + b.p + q.get(b).first + b.viaMember() + B<String>::p.name
    return if (result == "acccp") "OK" else result
}
