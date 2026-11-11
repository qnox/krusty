interface Check {
    fun accepts(value: Any?): Boolean
}

open class Base
class Derived : Base()
class Other

inline fun <reified T> checker(): Check = object : Check {
    override fun accepts(value: Any?) = value is T
}

fun box(): String {
    val bases = checker<Base>()
    if (!bases.accepts(Derived())) return "Derived is Base"
    if (bases.accepts(Other())) return "Other is not Base"
    if (bases.accepts(null)) return "null is not Base"
    return if (checker<Other?>().accepts(null)) "OK" else "null is Other?"
}
