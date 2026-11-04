interface First {
    fun first(): String
}

interface Second {
    fun second(): String
}

class Left : First, Second {
    override fun first() = "O"
    override fun second() = "K"
}

class Right : First, Second {
    override fun first() = "fail: right first"
    override fun second() = "fail: right second"
}

fun <T> pick(left: T, right: T, takeLeft: Boolean): T = if (takeLeft) left else right

fun box(): String {
    // `T` is inferred as `First & Second`, and its value is used as both.
    val both = pick(Left(), Right(), takeLeft = true)
    return both.first() + both.second()
}
