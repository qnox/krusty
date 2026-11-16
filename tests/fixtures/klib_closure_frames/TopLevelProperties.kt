package fixture

// Top-level properties whose accessors a consumer target lowers from the library's bodies. Each
// backing field is one storage location of the program, which the library's functions and the
// consumer read and write alike: a `var`, a `val`, a nullable `var` that starts `null`, a `const`,
// a property whose setter stores through `field`, and a computed property without a field.
var counter: Int = 0
val greeting: String = "hi"
var last: String? = null
const val LIMIT: Long = 3L
var clamped: Int = 0
    set(value) {
        field = if (value > 5) 5 else value
    }
val doubled: Int
    get() = counter * 2

fun bump(): Int {
    counter += 1
    return counter
}

fun remember(value: String): String? {
    val previous = last
    last = value
    return previous
}

fun limit(): Long = LIMIT

fun clamp(value: Int): Int {
    clamped = value
    return clamped
}
