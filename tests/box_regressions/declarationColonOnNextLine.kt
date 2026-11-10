// Kotlin's grammar allows a line break before the colon of a function or getter return type and of
// a secondary constructor's delegation call (`NL* ':'`), as it does before a supertype list.
open class Base(val value: String)

class Derived(value: String) : Base(value) {
    constructor(first: String, second: String)
        : this(first + second)
}

class Direct : Base {
    constructor(value: String)
        : super(value)
}

fun expressionBody(value: String)
    : String = value

fun blockBody(value: String)
    : String {
    return value
}

val getterValue: String
    get()
        : String = "K"

fun box(): String {
    val anonymous = fun(value: String)
        : String = value
    val derived = Derived("O", "K").value
    val direct = Direct("OK").value
    val result = expressionBody("O") + blockBody("K")
    val accessed = anonymous("O") + getterValue
    return if (derived == "OK" && direct == "OK" && result == "OK" && accessed == "OK") "OK"
    else "FAIL: $derived $direct $result $accessed"
}
