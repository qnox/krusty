// A `..` or `..<` over unsigned operands builds its range through the library's inline
// `rangeTo`/`rangeUntil`, which a target that does not lay out the range class expands.
fun uintRange(from: UInt, to: UInt): UIntRange = from..to

fun uintOpenRange(from: UInt, to: UInt): UIntRange = from..<to

fun ulongRange(from: ULong, to: ULong): ULongRange = from..to

fun ulongOpenRange(from: ULong, to: ULong): ULongRange = from..<to

fun box(): String {
    val closed = uintRange(2u, 5u)
    if (closed.first != 2u || closed.last != 5u) return "fail closed ${closed.first} ${closed.last}"
    val open = uintOpenRange(2u, 5u)
    if (open.first != 2u || open.last != 4u) return "fail open ${open.first} ${open.last}"
    val wide = ulongRange(0u, ULong.MAX_VALUE)
    if (wide.first != 0uL || wide.last != ULong.MAX_VALUE) return "fail wide"
    val wideOpen = ulongOpenRange(1u, 4u)
    if (wideOpen.first != 1uL || wideOpen.last != 3uL) return "fail wide open"
    if (!uintRange(3u, 1u).isEmpty()) return "fail empty"
    return "OK"
}
