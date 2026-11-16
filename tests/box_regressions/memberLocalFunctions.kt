// A local function declared in a member is a static function of the member's class that takes the
// captured receiver and locals as parameters. Calling it, referencing it, and nesting it must reach
// the same body whether it captures the receiver, a parameter, both, or neither.

class Host(val label: String) {
    fun direct(point: String): String {
        fun render(): String = "$label:$point"
        return render()
    }

    fun referenced(point: String): () -> String {
        fun render(): String = "$label:$point"
        return ::render
    }

    fun withArguments(prefix: String): String {
        fun render(suffix: String, twice: Boolean): String =
            prefix + (if (twice) label + label else label) + suffix
        val reference: (String, Boolean) -> String = ::render
        return render("!", false) + "|" + reference("?", true)
    }

    fun nested(): String {
        fun outer(): String {
            fun inner(): String = "inner:$label"
            return "outer(" + inner() + ")"
        }
        return outer()
    }

    fun capturesNothing(): Int {
        fun square(value: Int): Int = value * value
        return square(3) + square(4)
    }
}

class Box<T>(val value: T) {
    fun pick(other: T, first: Boolean): T {
        fun choose(): T = if (first) value else other
        val reference = ::choose
        return if (choose() == reference()) choose() else other
    }
}

object Registry {
    private var names = ""

    fun register(name: String): String {
        fun add() {
            names += name
        }
        add()
        return names
    }
}

fun box(): String {
    val host = Host("host")
    if (host.direct("p") != "host:p") return "FAIL direct: ${host.direct("p")}"
    val referenced = host.referenced("q")
    if (referenced() != "host:q") return "FAIL referenced: ${referenced()}"
    val arguments = host.withArguments(">")
    if (arguments != ">host!|>hosthost?") return "FAIL arguments: $arguments"
    if (host.nested() != "outer(inner:host)") return "FAIL nested: ${host.nested()}"
    if (host.capturesNothing() != 25) return "FAIL captures nothing: ${host.capturesNothing()}"
    val box = Box("a")
    if (box.pick("b", true) != "a" || box.pick("b", false) != "b") return "FAIL generic"
    Registry.register("x")
    if (Registry.register("y") != "xy") return "FAIL object"
    return "OK"
}
