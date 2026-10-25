// MODULE: lib
interface Greeter {
    fun greet(): String
}

class LibGreeter : Greeter {
    override fun greet(): String = "lib"
}

fun greetOne(greeter: Greeter): String = greeter.greet()

fun greetLib(): String = greetOne(LibGreeter())

// MODULE: main(lib)
class MainGreeter : Greeter {
    override fun greet(): String = "main"
}

fun box(): String {
    val lib = greetLib()
    if (lib != "lib") return "fail: $lib"
    val main = greetOne(MainGreeter())
    return if (main == "main") "OK" else "fail: $main"
}
