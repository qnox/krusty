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

fun box(): String {
    if (cast(Literal(Type())) != "plain" || cast("1") != "other") return "fallthrough"
    if (nullable(Literal(Type()), "x") != "x1") return "nullable"
    return cast(Literal(Json())) + nullable("1", null)
}
