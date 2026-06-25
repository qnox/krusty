// A top-level property of function type, invoked directly (`x()`): the call reads the property (the
// facade getter) and invokes it through `FunctionN.invoke`. Locals of function type already worked; this
// covers the top-level-property case (the foundation for `val x = ::foo; x()` function references).

val unit0: () -> String = { "O" }
val unary1: (String) -> String = { s -> s + "K" }

fun box(): String {
    return unary1(unit0())
}
