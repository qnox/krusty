// An unnamed local (`val _ = ...`) binds no name, so several of them share one scope.
// Each initializer is still evaluated, in order.

fun box(): String {
    val log = StringBuilder()
    val _ = log.append("O")
    val _: Int = log.append("K").length
    val _ = log.append("!").length
    return if (log.toString() == "OK!") "OK" else log.toString()
}
