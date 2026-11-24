// A callable reference passed to `this(...)` or `super(...)` takes its shape from the selected
// constructor's function-typed parameter, even when the referenced function is overloaded.
fun describe(value: Int): String = "int:$value"
fun describe(value: String): String = "string:$value"

open class Sink(val render: (String) -> String, val tag: String)

class Direct(render: (String) -> String) : Sink(render, "direct") {
    constructor() : this(::describe)
}

class Delegating : Sink {
    constructor(tag: String) : super(::describe, tag)
}

class Chained(val render: (Int) -> String, val label: String) {
    constructor(label: String) : this(::describe, label)
}

fun box(): String {
    val direct = Direct()
    if (direct.render("a") != "string:a") return "this(...): ${direct.render("a")}"
    val delegating = Delegating("t")
    if (delegating.render("b") != "string:b" || delegating.tag != "t") return "super(...): ${delegating.render("b")}"
    val chained = Chained("c")
    if (chained.render(3) != "int:3" || chained.label != "c") return "Int overload: ${chained.render(3)}"
    return "OK"
}
