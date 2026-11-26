// A stable value holding a call's result carries the call's `returnsNotNull()` and `returns(null)`
// effects: comparing the value with null proves what comparing the call would, for a local `val`
// and for a `val` property initialized in `init`.
import kotlin.contracts.*

interface Source {
    fun id(): String
}
class Selectable(val name: String) : Source {
    override fun id() = name
    fun select() = name
}
class Plain : Source {
    override fun id() = "p"
}

@OptIn(ExperimentalContracts::class)
fun selectorFor(source: Source): String? {
    contract { returnsNotNull() implies (source is Selectable) }
    return if (source is Selectable) source.name else null
}

@OptIn(ExperimentalContracts::class)
fun absentOf(source: Source?): String? {
    contract { returns(null) implies (source != null) }
    return if (source == null) "absent" else null
}

class Reader(private val source: Source) {
    private val selector: String?
    val selected: String

    init {
        selector = selectorFor(source)
        selected = if (selector != null) source.select() else "plain"
    }
}

fun local(source: Source): String {
    val selector = selectorFor(source)
    if (selector != null) return source.select()
    return "plain"
}

fun guarded(source: Source): String {
    val selector = selectorFor(source)
    if (null == selector) return "plain"
    return source.select()
}

fun absent(source: Source?): String {
    val absent = absentOf(source)
    return if (absent == null) source.id() else absent
}

fun box(): String {
    if (local(Selectable("a")) != "a" || local(Plain()) != "plain") return "local"
    if (guarded(Selectable("b")) != "b" || guarded(Plain()) != "plain") return "guarded"
    if (Reader(Selectable("c")).selected != "c" || Reader(Plain()).selected != "plain") {
        return "init"
    }
    if (absent(null) != "absent" || absent(Plain()) != "p") return "absent"
    return "OK"
}
