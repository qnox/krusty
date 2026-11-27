// A property delegates to a property reference whose target is a constant, a private or internal
// property, a generic class's property, an object's property, or a property read through a
// receiver that must be evaluated once, from members, top-level and local delegated properties.
// Reading and writing the delegating property reads and writes the referenced one, and a constant
// target reads as its value.
const val ANSWER = 42

val byConstant: Int by ::ANSWER

class Box<T>(var value: T)

class GenericOwner(val box: Box<String>) {
    var text: String by box::value
}

class Visibilities {
    private val hidden = "hidden"
    internal var inner = "inner"

    val shown by ::hidden
    var viaInner by ::inner
}

object Counter {
    var count = 0
}

var counted by Counter::count

var evaluations = 0

class Source(var name: String)

fun nextSource(): Source {
    evaluations++
    return Source("source")
}

class EvaluatedOnce {
    var name by nextSource()::name
}

open class Base {
    open val label = "base"
}

class Derived : Base() {
    override var label = "derived"
}

class OpenReceiver(base: Base) {
    val ref = base
    val label by ref::label
}

fun locals(): String {
    var local by Counter::count
    local = 7
    if (Counter.count != 7 || local != 7) return "FAIL local object: ${Counter.count}"
    val constant by ::ANSWER
    if (constant != 42) return "FAIL local constant: $constant"
    var stored by nextSource()::name
    stored = "local"
    if (stored != "local") return "FAIL local stored receiver: $stored"
    return "OK"
}

fun box(): String {
    if (byConstant != 42) return "FAIL constant: $byConstant"

    val owner = GenericOwner(Box("a"))
    owner.text = "b"
    if (owner.box.value != "b" || owner.text != "b") return "FAIL generic: ${owner.box.value}"

    val visibilities = Visibilities()
    if (visibilities.shown != "hidden") return "FAIL private: ${visibilities.shown}"
    visibilities.viaInner = "changed"
    if (visibilities.inner != "changed") return "FAIL internal: ${visibilities.inner}"

    counted = 5
    if (Counter.count != 5 || counted != 5) return "FAIL object: ${Counter.count}"

    val once = EvaluatedOnce()
    once.name = "renamed"
    if (once.name != "renamed") return "FAIL stored receiver: ${once.name}"
    if (evaluations != 1) return "FAIL receiver evaluations: $evaluations"

    val derived = Derived()
    val open = OpenReceiver(derived)
    derived.label = "changed"
    if (open.label != "changed") return "FAIL open receiver: ${open.label}"

    val local = locals()
    if (local != "OK") return local
    if (evaluations != 2) return "FAIL local receiver evaluations: $evaluations"
    return "OK"
}
