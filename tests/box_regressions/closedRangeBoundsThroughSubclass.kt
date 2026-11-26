// A class of the file that implements `ClosedRange` through a concrete base class is one dispatch
// arm with its subclass behind it, so a bound read of the interface type enters the getter for
// both and only what remains reaches the runtime's own range.
open class BaseRange(override val start: Int, override val endInclusive: Int) : ClosedRange<Int>

class DerivedRange : BaseRange(2, 5)

fun bounds(range: ClosedRange<Int>): String = "${range.start}..${range.endInclusive}"

fun box(): String {
    if (bounds(BaseRange(7, 9)) != "7..9") return "base"
    if (bounds(DerivedRange()) != "2..5") return "derived"
    if (bounds(3..7) != "3..7") return "runtime"
    return "OK"
}
