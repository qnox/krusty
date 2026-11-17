// WITH_STDLIB
import kotlin.jvm.JvmName

class Failure(val code: Int)

// Two generic overloads whose only difference is which formal their parameter passes through:
// `L : Any` is the more specific one, whatever its declaration ordinal.
class Box<out L : Any, out R> private constructor(val value: Any?) {
    companion object {
        fun <L : Any, R> make(value: Any?): Box<L, R> = Box(value)

        operator fun <L : Any, R> invoke(left: L): Box<L, R> = make(left)

        @JvmName("right1")
        operator fun <L : Any, R> invoke(right: R): Box<L, R> = make(right)
    }
}

class Factory {
    fun <L : Any, R> create(left: L): Box<L, R> = Box.make(left)

    @JvmName("createRight")
    fun <L : Any, R> create(right: R): Box<L, R> = Box.make(right)
}

fun <T : Any> viaCompanion(): Box<Failure, T> = Box(Failure(1))

fun <T : Any> viaMember(): Box<Failure, T> = Factory().create(Failure(2))

// A formal only another formal's bound mentions still takes part in the comparison: both overloads
// share the invariant `Holder<U>` bound, and `Marker` makes the first one stricter.
interface Marker

open class Holder<U>

class Both : Holder<String>(), Marker

fun <U, T> pick(x: T): String where T : Holder<U>, T : Marker = "marked"

@JvmName("pickPlain")
fun <U, T : Holder<U>> pick(x: T): String = "plain"

fun box(): String {
    val companion = viaCompanion<String>().value
    if (companion !is Failure || companion.code != 1) return "companion invoke: $companion"
    val member = viaMember<String>().value
    if (member !is Failure || member.code != 2) return "member: $member"
    if (pick(Both()) != "marked") return "bound transitive: ${pick(Both())}"
    return "OK"
}
