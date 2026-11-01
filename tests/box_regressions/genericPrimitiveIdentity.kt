// A primitive passed through a type parameter is carried as a reference and read back at its
// own type where the caller sees the substitution. Read as `Any`, the box keeps the primitive's
// own `equals`, and a `Boolean` box is one of two instances. A smart cast reads an `Int?` local's
// box back as an `Int` (kotlinc 2.4.21).
fun <T> id(value: T): T = value

class Holder<T>(val value: T)

fun same(a: Any, b: Any): Boolean = a == b

fun boxed(flag: Boolean): Any = flag

fun box(): String {
    if (id(-10L) != -10L) return "long"
    if (id(7) + 1 != 8) return "int"
    if (id('k') != 'k') return "char"
    if (!id(true)) return "boolean"
    if (id(1.5) != 1.5) return "double"
    if (id(2.5f) != 2.5f) return "float"
    val byte: Byte = 3
    if (id(byte) != byte) return "byte"
    val short: Short = -4
    if (id(short) != short) return "short"
    val held = Holder(40L)
    if (held.value + 2 != 42L) return "holder"
    if (!same(5, 5)) return "int equals"
    if (same(5, 5L)) return "an int box equals a long box"
    if (!same(-7L, -7L)) return "long equals"
    if (!same('c', 'c') || same('c', 'd')) return "char equals"
    if (same(0.0, -0.0)) return "double zero signs"
    if (!same(Double.NaN, 0.0 / 0.0)) return "double NaN"
    if (!same(Float.NaN, 0.0f / 0.0f)) return "float NaN"
    if (boxed(true) !== boxed(true) || boxed(false) === boxed(true)) return "boolean identity"
    var count: Int?
    count = 10
    val before = count++
    if (before != 10 || 11 != count) return "nullable increment"
    return "OK"
}
