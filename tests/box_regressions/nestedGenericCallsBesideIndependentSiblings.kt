// Two nested producers in one call each own their inference variables. `firstOf(none(), "O",
// none(), 'K')` solves `A` from `"O"` and `B` from `'K'` independently, and each `none()` is
// completed by the parameter it is passed to: one `Bag<String>`, the other `Bag<Char>`.
class Bag<T>(val size: Int)

fun <T> none(): Bag<T> = Bag(0)

fun <A, B> firstOf(a: Bag<A>, first: A, b: Bag<B>, second: B): A = first

fun <A, B> secondOf(a: Bag<A>, first: A, b: Bag<B>, second: B): B = second

fun box(): String {
    val o: String = firstOf(none(), "O", none(), 'K')
    val k: Char = secondOf(none(), "O", none(), 'K')
    if (o != "O" || k != 'K') return "independent"
    val fixed: Int = secondOf(Bag<Int>(1), 1, none(), 2)
    if (fixed != 2) return "fixed"
    return "OK"
}
