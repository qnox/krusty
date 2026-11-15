// A `Unit`-typed expression has the member scope of the `kotlin.Unit` object, so `Any` members
// declared there resolve as members, not only through `Any?` extensions.
fun unit() {}

fun <T : Unit> bounded(value: T): Boolean = value.equals(Unit)

fun box(): String {
    val u: Unit = Unit
    if (!u.equals(Unit)) return "Fail equals"
    if (u.equals(null)) return "Fail equals null"
    if (u.equals("Unit")) return "Fail equals other"
    if (!unit().equals(u)) return "Fail call result"
    if (u.hashCode() != Unit.hashCode()) return "Fail hashCode"
    if (u.toString() != "kotlin.Unit") return "Fail toString"
    val nullable: Unit? = if (u.hashCode() == 0 || true) Unit else null
    if (nullable?.equals(Unit) != true) return "Fail safe call"
    if (!bounded(Unit)) return "Fail bounded"
    return "OK"
}
