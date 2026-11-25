class Plain(val value: String)

class Foo {
    inner class Bar(val value: String)
}

fun box(): String {
    val plain: (String) -> Plain = ::Plain
    val inner: Foo.(String) -> Foo.Bar = Foo::Bar
    val bound: (String) -> Foo.Bar = Foo()::Bar
    if (bound("x").value != "x") return "fail"
    return plain("O").value + Foo().inner("K").value
}
