// A second safe cast of a value an earlier one narrowed tests the value itself: kotlinc stores it
// in the safe cast's temporary uncast and casts it only to the new target.
import kotlin.reflect.KMutableProperty1
import kotlin.reflect.KProperty
import kotlin.reflect.KProperty1

interface Named { val name: String }
interface Tagged : Named { val tag: String }

class Plain(override val name: String) : Named
class Labelled(override val name: String, override val tag: String) : Tagged

fun describe(value: Any): String {
    val named = value as? Named ?: return "none"
    val tagged = value as? Tagged ?: return "named ${named.name}"
    return "tagged ${tagged.name}:${tagged.tag}"
}

class Counter(var count: Int)

fun access(property: KProperty<*>): String {
    val read = property as? KProperty1<Counter, Int> ?: return "unreadable"
    val write = property as? KMutableProperty1<Counter, Int> ?: return "read ${read.get(Counter(3))}"
    val counter = Counter(1)
    write.set(counter, 7)
    return "written ${read.get(counter)}"
}

fun box(): String {
    val results = listOf(
        describe(1),
        describe(Plain("p")),
        describe(Labelled("l", "t")),
        access(Counter::count),
    )
    val expected = listOf("none", "named p", "tagged l:t", "written 7")
    return if (results == expected) "OK" else results.toString()
}
