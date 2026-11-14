// MODULE: lib
package fixture
var events: String = "I"
class Cell<T>(val value: T) { init { events += "C" } }
class Pair<T>(val first: T, val second: T?) { init { events += "P" } }
fun event(value: String): String { events += value; return value }
fun observed(): String = events
// MODULE: main(lib)
fun box(): String {
    if (fixture.Cell(42).value != 42) return "Int storage"
    if (fixture.observed() != "IC") return "initialization"
    val pair = fixture.Pair(second = fixture.event("K"), first = fixture.event("O"))
    if (pair.first != "O" || pair.second != "K") return "named arguments"
    if (fixture.Cell(Long.MIN_VALUE).value != Long.MIN_VALUE) return "Long storage"
    if (fixture.Cell<String?>(null).value != null) return "nullable storage"
    return if (fixture.observed() == "ICKOPCC") "OK" else "evaluation order"
}
