interface Marker { val binary: Boolean }
open class Type
class Json : Type(), Marker { override val binary = true }
class Literal(val type: Type)

fun cast(e: Any): String = when (e) {
    is Literal if e.type is Marker -> if (e.type.binary) "O" else "binary"
    is Literal -> "plain"
    else -> "other"
}

fun nullable(e: Any, other: String?): String = when (e) {
    is Literal if other != null -> if (other.length == 1) other + "1" else other
    else -> "K"
}

open class Base
open class Mid : Base()
class Leaf : Mid() { val n = 7 }
fun needLeaf(value: Leaf) = value.n
fun Base.answer() = when (this) {
    is Mid if this is Leaf -> needLeaf(this) + n
    is Mid -> 1
    else -> 0
}

fun box(): String {
    if (Leaf().answer() != 14 || Mid().answer() != 1 || Base().answer() != 0) return "receiver"
    if (cast(Literal(Type())) != "plain" || cast("1") != "other") return "fallthrough"
    if (nullable(Literal(Type()), "x") != "x1") return "nullable"
    return cast(Literal(Json())) + nullable("1", null)
}
