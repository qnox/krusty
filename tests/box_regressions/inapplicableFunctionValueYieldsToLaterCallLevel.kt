class Builder {
    var out = ""
    fun text(value: String) {
        out += value
    }
}

fun build(block: Builder.() -> Unit): String {
    val builder = Builder()
    builder.block()
    return builder.out
}

fun withDefault(
    text: Builder.() -> Unit,
    placeholder: Builder.() -> Unit = { text("p") },
): String = build {
    text()
    placeholder()
}

fun inBody(text: Builder.() -> Unit): String = build { text("b") }

fun label(value: String) = "top:$value"

fun topLevel(label: (Int) -> String): String = label("x") + label(1)

fun box(): String {
    if (withDefault({ text("t") }) != "tp") return "default value"
    if (inBody({}) != "b") return "body"
    if (topLevel { "value:$it" } != "top:xvalue:1") return "top level"
    return "OK"
}
