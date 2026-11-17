// A vararg parameter joined through the stdlib, whose body appends each element by its runtime
// type, followed by a parameter with a default value.

fun join(vararg values: String, suffix: String = "!"): String = values.joinToString("") + suffix

fun box(): String {
    if (join() != "!") return "Fail default"
    if (join("O", "K") != "OK!") return "Fail positional vararg"
    if (join(*arrayOf("O", "K"), suffix = "!") != "OK!") return "Fail spread vararg"
    if (join("O", suffix = "K") != "OK") return "Fail named suffix"
    if (arrayOf('O', 'K').joinToString("") != "OK") return "Fail chars"
    return "OK"
}
