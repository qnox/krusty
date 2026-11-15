// WITH_STDLIB
OPTIONAL_JVM_INLINE_ANNOTATION
value class Meter(val v: Int)
fun box(): String {
    if (Meter(3) != Meter(3)) return "meter equality"
    if (Meter(3).hashCode() != Meter(3).hashCode()) return "meter hash"
    if (Meter(3).hashCode() == Meter(4).hashCode()) return "meter hash collision"
    val meters: Array<Meter?> = Array(2) { null }
    meters[1] = Meter(3)
    if (meters[0] != null) return "meters[0]"
    if (meters[1]?.v != 3) return "meters[1]"
    val longs: Array<ULong?> = Array(1) { null }
    longs[0] = 2uL
    if (longs[0] != 2uL) return "longs[0]"
    val units: Array<Unit?> = Array(1) { null }
    units[0] = Unit
    if (units[0] != Unit) return "units[0]"
    return "OK"
}
