// A failed reference cast throws: a value of another type a ClassCastException, a null cast to a
// non-null type a NullPointerException.
interface Shape { fun name(): String }
open class Square : Shape { override fun name(): String = "square" }
class Other

fun asShape(value: Any?): Shape = value as Shape
fun asSquare(value: Any?): Square = value as Square
fun asNullableShape(value: Any?): Shape? = value as Shape?
fun <T : Any> asErased(value: Any?): T = value as T

fun box(): String {
    if (asShape(Square()).name() != "square" || asSquare(Square()).name() != "square") return "valid"
    if (asNullableShape(null) != null) return "nullable"
    try { asShape(Other()); return "interface accepted other" }
    catch (expected: ClassCastException) {}
    try { asSquare(Other()); return "class accepted other" }
    catch (expected: ClassCastException) {}
    try { asNullableShape(Other()); return "nullable interface accepted other" }
    catch (expected: ClassCastException) {}
    try { asShape(null); return "interface accepted null" }
    catch (expected: NullPointerException) {}
    try { asSquare(null); return "class accepted null" }
    catch (expected: NullPointerException) {}
    try { asErased<Any>(null); return "erased accepted null" }
    catch (expected: NullPointerException) {}
    return "OK"
}
