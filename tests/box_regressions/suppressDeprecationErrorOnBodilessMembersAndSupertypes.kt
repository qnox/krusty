// WITH_STDLIB
@Deprecated("gone", level = DeprecationLevel.ERROR)
open class Old(val tag: String)

interface Visitor<R> {
    @Suppress("DEPRECATION_ERROR")
    fun visit(message: Old): R

    @Suppress("DEPRECATION_ERROR")
    fun produce(): Old

    @Suppress("DEPRECATION_ERROR")
    val current: Old
}

abstract class Base {
    @Suppress("DEPRECATION_ERROR")
    abstract fun accept(message: Old): String
}

class Sub : @Suppress("DEPRECATION_ERROR") Old("sub")

class Impl : Visitor<String> {
    @Suppress("DEPRECATION_ERROR")
    override fun visit(message: Old): String = message.tag

    @Suppress("DEPRECATION_ERROR")
    override fun produce(): Old = Sub()

    @Suppress("DEPRECATION_ERROR")
    override val current: Old = Sub()
}

fun box(): String {
    val impl = Impl()
    return if (impl.visit(impl.produce()) == "sub" && impl.current.tag == "sub") "OK" else "FAIL"
}
