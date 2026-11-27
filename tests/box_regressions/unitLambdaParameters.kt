// WITH_STDLIB

fun interface UnitSink {
    fun accept(value: Unit): String
}

fun <T> pass(value: T, f: (T) -> String): String = f(value)

fun sideEffect(log: StringBuilder): Unit {
    log.append("side;")
}

fun box(): String {
    val log = StringBuilder()
    val ignore: (Unit) -> Unit = { log.append("ignore;") }
    ignore(Unit)
    ignore(sideEffect(log))

    val describe: (Unit) -> String = { "unit" }
    val viaGeneric = pass(Unit, describe)
    val viaLiteral = pass(Unit) { it.toString() }

    val sink = UnitSink { "sink" }
    val viaSam = sink.accept(Unit)

    val result = "$log$viaGeneric,$viaLiteral,$viaSam"
    return if (result == "ignore;side;ignore;unit,kotlin.Unit,sink") "OK" else result
}
