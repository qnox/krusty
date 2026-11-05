// A library declaration overriding one of `kotlin.Any`'s members (`String.hashCode`, `equals`,
// `toString`) answers through its receiver, called directly or through an `Any` receiver,
// including a boxed scalar, a nullable receiver behind a safe call, and `Unit`.
data class Box(val label: String)
fun box(): String {
    val text = "ab"
    if (text.hashCode() != 3105) return "fail string hash ${text.hashCode()}"
    if (!text.equals("a" + "b") || text.equals("ba")) return "fail string equals"
    if (text.toString() != "ab") return "fail string toString"
    val any: Any = text
    if (any.hashCode() != 3105) return "fail Any hash"
    if (!any.equals("ab")) return "fail Any equals"
    val label = Box("x")
    if (label.hashCode() != "x".hashCode()) return "fail data hash"
    if (!label.equals(Box("x"))) return "fail data equals"
    val boxed: Any = 5
    if (boxed.hashCode() != 5 || !boxed.equals(5)) return "fail boxed Int"
    if (label.toString() != "Box(label=x)") return "fail data toString"
    val present: String? = "ab"
    val absent: String? = null
    if (present?.hashCode() != 3105 || absent?.hashCode() != null) return "fail nullable hash"
    if (Unit.hashCode() != Unit.hashCode() || Unit.toString() != "kotlin.Unit") return "fail Unit"
    return "OK"
}
