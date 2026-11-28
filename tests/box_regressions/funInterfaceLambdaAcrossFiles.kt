// A lambda converted to a `fun interface` that another file declares: the object answers the
// converted member, through the interface and through a parent fun interface it refines, and
// passes type checks against both. A generic fun interface converts the same way.
// FILE: interfaces.kt

fun interface Transform {
    fun apply(value: Int): Int
}

fun interface Scaler : Transform

fun interface Mapper<T, R> {
    fun map(value: T): R
}

// FILE: box.kt

fun box(): String {
    val factor = 2
    val scaler = Scaler { it * factor }
    if (scaler.apply(4) != 8) return "Fail apply: ${scaler.apply(4)}"
    val transform: Transform = scaler
    if (transform.apply(5) != 10) return "Fail parent apply: ${transform.apply(5)}"
    val any: Any = scaler
    if (any !is Scaler || any !is Transform) return "Fail is"
    val mapper = Mapper<Int, String> { "v$it" }
    if (mapper.map(3) != "v3") return "Fail map: ${mapper.map(3)}"
    return "OK"
}
