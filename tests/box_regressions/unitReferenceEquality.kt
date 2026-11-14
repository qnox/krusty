private var trace = ""
private fun unitValue(mark: String) { trace += mark }
private fun erasedUnit(mark: String): Any { trace += mark; return Unit }

fun box(): String {
    if (unitValue("a") !== erasedUnit("b")) return "left Unit"
    if (trace != "ab") return "left evaluation: $trace"
    if (erasedUnit("c") !== unitValue("d")) return "right Unit"
    if (trace != "abcd") return "right evaluation: $trace"
    if (unitValue("e") !== unitValue("f")) return "two Unit values"
    if (trace != "abcdef") return "both evaluation: $trace"
    val nullable: Unit? = Unit
    if (nullable !== unitValue("g")) return "nullable Unit"
    if (trace != "abcdefg") return "nullable evaluation: $trace"
    val absent: Unit? = null
    if (absent === unitValue("h")) return "null Unit"
    if (trace != "abcdefgh") return "null evaluation: $trace"
    return "OK"
}
