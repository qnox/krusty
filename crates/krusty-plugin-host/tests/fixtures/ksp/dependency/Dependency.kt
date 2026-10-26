package dependency

@Repeatable
@Retention(AnnotationRetention.RUNTIME)
annotation class Tag(val v: String)

@Tag("a")
@Tag("b")
class Tagged

@Tag("c")
class OneTag

@Deprecated("old")
class Old

@JvmInline
value class Wrapper(val value: Int)

sealed class Empty

sealed interface Shape

class Circle : Shape

class Holder<A> {
    inner class Inner<B>

    inner class Bare

    class Nested<C>
}
