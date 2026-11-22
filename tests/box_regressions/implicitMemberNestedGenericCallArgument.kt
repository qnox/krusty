// A bare call to a member of an implicit receiver takes its nested generic call's type arguments
// from the member's parameter, as `this.member(...)` does, while the enclosing function's return
// type is still being inferred.

interface Declaration {
    fun asType(arguments: List<String>): String
}

fun Declaration.asType() = asType(emptyList())

class Plain {
    fun names(items: List<String>): Int = items.size

    fun count() = names(emptyList())

    fun viaThis() = this.names(emptyList())

    fun listed() = names(listOf())
}

class Impl : Declaration {
    override fun asType(arguments: List<String>) = "T" + arguments.size
}

fun box(): String {
    val plain = Plain()
    if (plain.count() != 0 || plain.viaThis() != 0 || plain.listed() != 0) return "fail: plain"
    if (Impl().asType() != "T0") return "fail: extension"
    return "OK"
}
