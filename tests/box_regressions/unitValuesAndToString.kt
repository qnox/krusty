// `Unit`'s one instance is a value like any other: it is stored, returned, tested and printed,
// and `toString()` on a nullable or primitive receiver is the string a template makes of it.
class UnitSlot(val value: Unit) {
    fun read(): Unit = value
}

class Route(val value: String) {
    constructor(value: Int) : this(value.toString())
}

fun describe(value: Int?): String = value.toString()

fun box(): String {
    val slot = UnitSlot(Unit)
    if (slot.read() !is Unit) return "fail returned Unit"
    if (slot.value !is Unit) return "fail stored Unit"
    if ("${slot.read()}" != "kotlin.Unit") return "fail Unit template: ${slot.read()}"
    if (Route(42).value != "42") return "fail Int toString"
    if (describe(null) != "null" || describe(7) != "7") return "fail nullable toString"
    return "OK"
}
