// A recursive call owns fresh inference variables even though the called declaration's own formal
// is in scope as the enclosing fixed type. `count(depth - 1, depth, none())` solves its `T` to
// `Int`, while `count(depth - 1, head, none())` solves it to the enclosing `T`; each nested `none()`
// is completed by the call it is an argument of, never by the enclosing declaration's `T`.
class Bag<T>(val size: Int)

fun <T> none(): Bag<T> = Bag(0)

fun <T> count(depth: Int, head: T, rest: Bag<T>): Int =
    if (depth == 0) rest.size + 1
    else count(depth - 1, depth, none()) + count(depth - 1, head, none())

fun box(): String = if (count(2, "s", none()) == 4) "OK" else "count"
