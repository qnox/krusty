// WITH_STDLIB
interface Tag

class Failure(val code: Int) : Tag

// Two generic overloads whose only difference is which formal their parameter passes through:
// `L : Tag` is the more specific one, whatever its declaration ordinal. Their erasures differ
// (`Tag` against `Object`), so neither needs a platform name.
class Box<out L : Tag, out R> private constructor(val value: Any?) {
    companion object {
        fun <L : Tag, R> make(value: Any?): Box<L, R> = Box(value)

        operator fun <L : Tag, R> invoke(left: L): Box<L, R> = make(left)

        operator fun <L : Tag, R> invoke(right: R): Box<L, R> = make(right)
    }
}

class Factory {
    fun <L : Tag, R> create(left: L): Box<L, R> = Box.make(left)

    fun <L : Tag, R> create(right: R): Box<L, R> = Box.make(right)
}

fun <T> viaCompanion(): Box<Failure, T> = Box(Failure(1))

fun <T> viaMember(): Box<Failure, T> = Factory().create(Failure(2))

// A formal only another formal's bound mentions still takes part in the comparison: both overloads
// share the invariant `Holder<U>` bound, and `Marker` makes the first one stricter.
interface Marker

interface Holder<U>

class Both : Holder<String>, Marker

fun <U, T> pick(x: T): String where T : Marker, T : Holder<U> = "marked"

fun <U, T : Holder<U>> pick(x: T): String = "plain"

fun box(): String {
    val companion = viaCompanion<String>().value
    if (companion !is Failure || companion.code != 1) return "companion invoke: $companion"
    val member = viaMember<String>().value
    if (member !is Failure || member.code != 2) return "member: $member"
    if (pick(Both()) != "marked") return "bound transitive: ${pick(Both())}"
    return "OK"
}
