fun box(): String {
    if (tableAt(2) != 4) return "table"
    if (initializationSteps() != 2) return "initialized once: ${initializationSteps()}"
    if (order() != 112) return "order: ${order()}"
    if (earlyRead() != 0) return "a forward read sees the default"
    if (advance() != 4) return "first advance"
    if (advance() != 3) return "second advance"
    if (initializationSteps() != 2) return "initialized again"
    return "OK"
}
