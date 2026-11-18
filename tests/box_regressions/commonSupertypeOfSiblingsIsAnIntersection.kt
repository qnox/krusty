interface Named {
    val name: String
}

abstract class Expression<T>

abstract class TypedExpression<T> : Expression<T>()

class PlainAlias<T>(override val name: String) : Expression<T>(), Named

class TypedAlias<T>(override val name: String) : TypedExpression<T>(), Named

class Builder {
    operator fun Expression<*>.unaryPlus(): String = "expression"

    fun render(typed: Boolean): String {
        val alias = if (typed) TypedAlias<Int>("t") else PlainAlias<String>("p")
        return alias.name + ":" + (+alias)
    }

    fun aliases(typed: Boolean): List<String> {
        val chosen = listOf(typed).map { if (it) TypedAlias<Int>("t") else PlainAlias<String>("p") }
        return chosen.map { it.name + ":" + (+it) }
    }
}

fun box(): String {
    val builder = Builder()
    if (builder.render(true) != "t:expression") return "typed"
    if (builder.render(false) != "p:expression") return "plain"
    if (builder.aliases(true) != listOf("t:expression")) return "collection"
    return "OK"
}
