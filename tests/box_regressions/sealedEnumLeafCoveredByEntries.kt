sealed interface Base
class Leaf : Base
enum class Mode : Base { First, Second }

fun describe(base: Base): String = when (base) {
    is Leaf -> "leaf"
    Mode.First -> "first"
    Mode.Second -> "second"
}

fun wholeEnum(base: Base): String = when (base) {
    is Leaf -> "leaf"
    is Mode -> "mode"
}

fun box(): String {
    if (describe(Leaf()) != "leaf") return "Leaf"
    if (describe(Mode.First) != "first") return "First"
    if (describe(Mode.Second) != "second") return "Second"
    if (wholeEnum(Mode.Second) != "mode") return "Mode"
    return "OK"
}
