fun two(a: Int, b: Int): Int = a + b

fun interface Supplier {
    fun get(): Int
}

// The text is target-specific and ends in an identity hash, so only that every function value
// renders something is pinned.
fun box(): String {
    val lambda: Any = { x: Int -> x }
    val reference: Any = ::two
    val supplier: Any = Supplier { 1 }
    if (lambda.toString().length <= 8) return "lambda"
    if (reference.toString().length <= 8) return "reference"
    if (supplier.toString().length <= 8) return "supplier"
    return "OK"
}
