// A range compares, hashes and tests membership as its progression does: the 2.5 standard library
// implements `IntRange`, `LongRange` and `CharRange` equality and hashing by a `super` call of
// their progression's member, which a target reads from the library's own serialized bodies.
fun box(): String {
    val ints = 1..3
    val longs = 1L..3L
    val chars = 'a'..'c'
    if (ints != 1..3 || longs != 1L..3L || chars != 'a'..'c') return "equal ranges differ"
    if (ints == 1..4 || longs == 0L..3L || chars == 'a'..'d') return "different ranges are equal"
    if (1..0 != 5..4) return "empty ranges differ"
    if (ints.hashCode() != (1..3).hashCode()) return "equal int ranges hash apart"
    if (longs.hashCode() != (1L..3L).hashCode()) return "equal long ranges hash apart"
    if (chars.hashCode() != ('a'..'c').hashCode()) return "equal char ranges hash apart"
    val x = 2
    val inside = when (x) {
        in ints -> "in"
        else -> "out"
    }
    if (inside != "in" || 4L in longs || 'b' !in chars) return "membership: $inside"
    return "OK"
}
