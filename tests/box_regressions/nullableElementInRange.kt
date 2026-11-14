// A nullable value over a range of its own element type resolves to that element's precise
// `contains` overload, so membership holds for a present value and fails for `null`.
val nullableInt: Int? = 5
val nullableLong: Long? = 5L
val nullableChar: Char? = 'c'
val nothing: Int? = null

fun box(): String {
    if (nullableInt !in 0..10) return "Int? in range"
    if (nullableInt in 6..10) return "Int? above range"
    if (nothing in 0..10) return "null in range"
    if (nothing !in 0..10 == false) return "null not in range"
    if (nullableLong !in 0L..10L) return "Long? in range"
    if (nullableChar !in 'a'..'z') return "Char? in range"
    if (nullableChar in 'x'..'z') return "Char? above range"
    return "OK"
}
