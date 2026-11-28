package fixture

fun box(): String {
    val source = emptySource()
    if (source.label() != "source") return "label = ${source.label()}"
    if (source.item() != "none") return "item = ${source.item()}"
    return "OK"
}
