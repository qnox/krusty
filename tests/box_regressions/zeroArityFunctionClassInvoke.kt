// WITH_STDLIB
// A class implementing a function type, called through a function value of that type: the call
// site knows only the arity, so the class must answer the same call a lambda of that arity does.

class Greeting(val text: String) : () -> String {
    override fun invoke(): String = text
}

class Suffix(val suffix: String) : (String) -> String {
    override fun invoke(prefix: String): String = prefix + suffix
}

fun call0(f: () -> String): String = f()

fun call1(f: (String) -> String, argument: String): String = f(argument)

fun box(): String {
    val zero: () -> String = Greeting("O")
    if (zero() != "O") return "class arity 0: ${zero()}"
    if (call0(Greeting("K")) != "K") return "class arity 0 argument"
    val one: (String) -> String = Suffix("K")
    if (one("O") != "OK") return "class arity 1: ${one("O")}"
    if (call1(Suffix("K"), "O") != "OK") return "class arity 1 argument"
    val anonymous: () -> String = object : () -> String {
        override fun invoke() = "OK"
    }
    if (anonymous() != "OK") return "object arity 0: ${anonymous()}"
    return "OK"
}
