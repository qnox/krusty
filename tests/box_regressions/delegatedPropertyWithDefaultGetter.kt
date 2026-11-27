interface Named {
    val name: String
        get() = "default"
    val size: Int
}

class Impl : Named {
    override val name: String = "impl"
    override val size: Int = 3
}

class Delegating(n: Named) : Named by n

fun box(): String {
    val d = Delegating(Impl())
    return if (d.name == "impl" && d.size == 3) "OK" else "fail"
}
