// A lambda converted to a `fun interface` is an instance of it and implements its member, with
// the member's own parameter and result types, including generic ones and captured values.
fun interface IntOp { fun apply(x: Int): Int }
fun interface Transform<T> { fun go(x: T): T }

fun take(op: IntOp) = op.apply(2)

fun box(): String {
    val direct = IntOp { it + 3 }
    if (direct.apply(1) != 4) return "direct"
    if (take { it * 5 } != 10) return "argument"
    val generic = Transform<String> { it + "K" }
    if (generic.go("O") != "OK") return "generic"
    val offset = 7
    val captured: Any = IntOp { it + offset }
    if (captured !is IntOp || captured.apply(1) != 8) return "captured"
    return "OK"
}
