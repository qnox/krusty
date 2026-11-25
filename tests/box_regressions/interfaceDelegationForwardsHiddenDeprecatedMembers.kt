// WITH_STDLIB
interface Log {
    val id: String?
    fun info(message: String?)
    fun info(e: Throwable?): Unit = info(null as String?)

    @Deprecated("hidden", level = DeprecationLevel.HIDDEN)
    var follower: Log?
        get() = null
        set(value) {}

    @Deprecated("hidden", level = DeprecationLevel.HIDDEN)
    operator fun <T : Log> plus(follower: T): T = follower
}

interface Marked : Log {
    fun sub(name: String): Marked
}

class Impl(override val id: String?) : Log {
    var last = ""
    override fun info(message: String?) {
        last = message ?: "null"
    }
}

private class Wrapper(val origin: Log) : Log by origin, Marked {
    override fun sub(name: String): Marked = Wrapper(origin)
}

fun box(): String {
    val impl = Impl("x")
    val wrapper = Wrapper(impl)
    wrapper.info("hello")
    wrapper.sub("child").info(null as Throwable?)
    return if (impl.last == "null" && wrapper.id == "x") "OK" else "FAIL: ${impl.last} ${wrapper.id}"
}
