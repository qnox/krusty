// An implicitly typed property's initializer is resolved with its header. Between a generic
// declaration and a non-generic one that the call instantiates to the same parameters, kotlinc
// selects the non-generic one: `make(0)` is `make(Int)`, not `make<Int>(T)`. Both shapes of the
// family are pinned: one whose declarations map the arguments alike, and one whose declarations
// also leave a default parameter out.
class Generic<T>(val value: T)
class Concrete(val value: Int)

fun <T> make(initial: T): Generic<T> = Generic(initial)
fun make(initial: Int): Concrete = Concrete(initial * 10)

fun <T> withDefault(initial: T, label: String = "generic"): Generic<T> = Generic(initial)
fun <T> withDefault(initial: T): Generic<T> = Generic(initial)
fun withDefault(initial: Int, label: String = "concrete"): Concrete = Concrete(initial * 100)
fun withDefault(initial: Int): Concrete = Concrete(initial * 10)

val topShared = make(1)
val topSeparate = withDefault(2)

class Holder {
    private val shared = make(3)
    private val separate = withDefault(4)
    val generic = make("five")

    fun sum(): Int = shared.value + separate.value + generic.value.length
}

fun box(): String {
    val result = topShared.value + topSeparate.value + Holder().sum()
    return if (result == 104) "OK" else "fail: $result"
}
