fun box(): String {
    val base = 40
    val strings = probe<String>("strings") { base + 2 }
    if (strings.label() != "strings") return "label ${strings.label()}"
    if (!strings.accepts("text") || strings.accepts(7)) return "String type test"
    if (strings.run() != 42) return "run ${strings.run()}"
    val ints = probe<Int>("ints") { base - 1 }
    if (ints.label() != "ints") return "second label ${ints.label()}"
    if (!ints.accepts(7) || ints.accepts("text")) return "Int type test"
    if (ints.run() != 39 || strings.run() != 42) return "second run ${ints.run()}"
    return "OK"
}
