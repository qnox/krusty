// An argument fits a parameter declared as the candidate's own type variable exactly only because
// inference bound the variable to the argument's type. That is no evidence against another
// applicable overload: kotlinc compares the declarations, so `check(x: T)` loses to the more
// specific `check(x: Box<E>?)`, `check(x: Shape)` and `check(x: Any)`, for a nullable or subtype
// argument alike, and for top-level, member and extension candidates.
open class Shape
class Circle : Shape()
open class Box<E>(val value: E)
class Crate<E>(value: E) : Box<E>(value)

fun <T> check(x: T): Int = 1
fun <E> check(x: Box<E>?): Int = 2

fun <T> fit(x: T): Int = 1
fun fit(x: Shape): Int = 2

fun <T> top(x: T): Int = 1
fun top(x: Any): Long = 2L

object Asserts {
    fun <T> check(x: T): Int = 1
    fun <E> check(x: Box<E>?): Int = 2
}

fun <T> String.tag(x: T): Int = 1
fun <E> String.tag(x: Box<E>?): Int = 2

fun <T> pair(x: T, y: T): Int = 1
fun <E> pair(x: Box<E>?, y: Box<E>?): Int = 2

fun box(): String {
    val nullable: Box<String>? = Box("a")
    val absent: Box<String>? = null
    val crate = Crate("a")
    val nullableCrate: Crate<String>? = crate
    if (check(nullable) != 2) return "nullable argument"
    if (check(absent) != 2) return "null-valued nullable argument"
    if (check(crate) != 2) return "subtype argument"
    if (check(nullableCrate) != 2) return "nullable subtype argument"
    if (check(Box(1)) != 2) return "exact argument"
    if (check("s") != 1) return "only the variable applies"
    if (fit(Circle()) != 2) return "non-generic supertype"
    if (top("s") != 2L) return "non-generic Any"
    if (Asserts.check(nullable) != 2) return "member"
    if ("s".tag(nullableCrate) != 2) return "extension"
    if (pair(nullable, crate) != 2) return "two arguments"
    return "OK"
}
