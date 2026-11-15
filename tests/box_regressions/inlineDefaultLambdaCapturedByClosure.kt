// An inline function's default lambda is a method of the declaration's `$default` stub, so
// expanding a call that uses the default must leave its body in place even when the function
// wraps the parameter in a closure of its own.

fun run(block: () -> String): String = block()

inline fun wrapped(noinline block: () -> String = { "O" }): String = run { block() }

inline fun wrappedCrossinline(crossinline block: () -> String = { "K" }): String = run { block() }

fun box(): String {
    if (wrapped() != "O") return "FAIL default"
    if (wrapped { "x" } != "x") return "FAIL explicit"
    if (wrappedCrossinline() != "K") return "FAIL crossinline default"
    if (wrappedCrossinline { "y" } != "y") return "FAIL crossinline explicit"
    val stub: () -> String = { "S" }
    return if (wrapped(stub) == "S") "OK" else "FAIL reference"
}
