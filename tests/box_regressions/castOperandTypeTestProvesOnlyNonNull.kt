// A type test of a cast `x as T` proves only that `x` is not null: kotlinc's data flow carries no
// other fact from the cast's result back to `x`. `require(a as Any is B)` leaves `a` an `A`, so
// the extension declared on `A` is selected, while `(s as Any?) is String` makes `s` non-null.
open class A

class B : A()

fun A.pick(): String = "A"
fun B.pick(): String = "B"

fun viaCast(a: A): String {
    require(a as Any is B)
    return a.pick()
}

fun nonNull(s: CharSequence?): Int = if ((s as Any?) is String) s.length else -1

fun box(): String {
    val picked = viaCast(B())
    val length = nonNull("abc")
    val absent = nonNull(null)
    return if (picked == "A" && length == 3 && absent == -1) "OK" else "FAIL: $picked $length $absent"
}
