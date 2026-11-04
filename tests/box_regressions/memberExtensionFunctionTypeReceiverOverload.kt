// A formal that a member extension's receiver reaches through function types is solved from every
// occurrence at its composed variance: results read covariantly, parameters contravariantly.
class Duo<out A, out B>(val first: A, val second: B)

open class Shape
class Circle : Shape()
class Square : Shape()

class Log {
    var text = ""
}

object Extensions {
    fun <T> Duo<() -> T, () -> T>.firstResult(): T = first()

    fun inferredResult(pair: Duo<() -> Circle, () -> Square>): Any = pair.firstResult()

    fun explicitResult(pair: Duo<() -> Circle, () -> Square>): Any = pair.firstResult<Any>()

    fun <T> Duo<(T) -> Unit, (T) -> Unit>.invokeBoth(value: T) {
        first(value)
        second(value)
    }

    fun inferredParameters(pair: Duo<(Any) -> Unit, (String) -> Unit>) {
        pair.invokeBoth("K")
    }

    fun explicitParameters(pair: Duo<(Any) -> Unit, (String) -> Unit>) {
        pair.invokeBoth<String>("K")
    }
}

fun box(): String {
    val circle = Circle()
    val results: Duo<() -> Circle, () -> Square> = Duo({ circle }, { Square() })
    if (Extensions.inferredResult(results) !== circle) return "inferred result"
    if (Extensions.explicitResult(results) !== circle) return "explicit result"
    val log = Log()
    val parameters: Duo<(Any) -> Unit, (String) -> Unit> =
        Duo({ log.text += "O" }, { log.text += it })
    Extensions.inferredParameters(parameters)
    Extensions.explicitParameters(parameters)
    return if (log.text == "OKOK") "OK" else log.text
}
