class UnitSlot(val value: Unit) {
    fun component1(): Unit = value
    fun copy(): UnitSlot = UnitSlot(value)
}

class SelectedRoute(val value: String) {
    constructor(value: Int) : this(value.toString())
}

fun box(): String {
    val unit = UnitSlot(Unit)
    if (unit.component1() !is Unit) return "fail primary Unit parameter"
    if (unit.copy().value !is Unit) return "fail copied Unit parameter"
    if (SelectedRoute(42).value != "42") return "fail selected secondary"
    if (SelectedRoute("OK").value != "OK") return "fail primary String parameter"
    return "OK"
}
