// Kotlin's answers for `print_of_raising_to_string_writes_nothing.c`: `print` and `println` whose
// `toString` raises propagate the program's exception; the driver's stdout also shows that neither
// call wrote a byte.
class Boom : Exception("boom")

class Unprintable {
    override fun toString(): String = throw Boom()
}

fun box(): String {
    val out = StringBuilder()
    val u = Unprintable()
    try {
        print(u)
    } catch (e: Throwable) {
        out.append("print = threw ${e::class.simpleName}\n")
    }
    try {
        println(u)
    } catch (e: Throwable) {
        out.append("println = threw ${e::class.simpleName}\n")
    }
    return out.toString()
}
