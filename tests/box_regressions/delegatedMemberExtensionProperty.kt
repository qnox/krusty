interface Scope {
    val String.twice: String
    fun String.shout(): String
}

class Impl : Scope {
    override val String.twice: String get() = this + this
    override fun String.shout(): String = this + "!"
}

class Delegating(scope: Scope) : Scope by scope

fun box(): String {
    val d = Delegating(Impl())
    return with(d) {
        if ("o".shout() != "o!") return "shout"
        if ("k".twice != "kk") return "twice"
        "OK"
    }
}
