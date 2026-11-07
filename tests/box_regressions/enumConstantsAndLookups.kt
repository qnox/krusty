// An enum realized as ordinary statics and constructions: constants built once, in declaration
// order, with `name` and `ordinal` filled before their constructors run; `values()` fresh on every
// call; `valueOf` by name, failing with IllegalArgumentException.

var built = ""

enum class Size(val weight: Int = 10) {
    SMALL(1),
    MEDIUM,
    LARGE(100) {
        override fun label() = "big"
    };

    val seen = "$name@$ordinal"

    init {
        built += name
    }

    open fun label() = name
}

fun box(): String {
    if (built != "") return "constants are built before the enum is used: $built"
    if (Size.MEDIUM.ordinal != 1) return "ordinal"
    if (built != "SMALLMEDIUMLARGE") return "every constant is built on first use, in order: $built"
    if (Size.MEDIUM.weight != 10) return "a default argument"
    if (Size.LARGE.weight != 100) return "a body constant's argument"
    if (Size.LARGE.label() != "big") return "a body constant's override"
    if (Size.SMALL.label() != "SMALL") return "the name"
    if (Size.LARGE.seen != "LARGE@2") return "name and ordinal during construction: ${Size.LARGE.seen}"

    val first = Size.values()
    val second = Size.values()
    if (first === second) return "values() is a fresh array"
    if (first[1] !== Size.MEDIUM || first[2] !== Size.LARGE) return "values() order"

    if (Size.valueOf("MEDIUM") !== Size.MEDIUM) return "valueOf"
    val missing = try {
        Size.valueOf("HUGE")
        "no failure"
    } catch (e: Throwable) {
        if (e is IllegalArgumentException) "IllegalArgumentException" else "another failure"
    }
    if (missing != "IllegalArgumentException") return "valueOf of a missing name: $missing"

    if (built != "SMALLMEDIUMLARGE") return "the enum is built once: $built"
    return "OK"
}
