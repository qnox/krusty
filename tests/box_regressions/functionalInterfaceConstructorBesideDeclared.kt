// A functional interface's constructor in a closer scope wins over a same-named class's declared
// constructor further out, and the declared constructor still answers outside that scope.

class Action(val f: () -> String) {
    fun run(): String = "declared " + f()
}

object Holder {
    fun interface Action {
        fun run(): String
    }

    fun make(): String = Action { "sam" }.run()
}

fun box(): String {
    val inner = Holder.make()
    val outer = Action { "lambda" }.run()
    return if (inner == "sam" && outer == "declared lambda") "OK" else "inner $inner, outer $outer"
}
