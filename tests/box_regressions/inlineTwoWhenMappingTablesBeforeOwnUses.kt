// A caller declared before an inline function with two `$WhenMappings` tables, in a file whose
// later own `when` claims a third enum first: the callee's tables are numbered after the caller's
// own, and the inlined code still reads the callee's tables.
enum class A { X, Y }
enum class B { P, Q }
enum class E { M, N }

fun caller(a: A, b: B, e: E): Int = twoTables(a, b) + when (e) { E.N -> 100; E.M -> 200 }

inline fun twoTables(a: A, b: B): Int =
    when (b) { B.Q -> 1; B.P -> 2 } + when (a) { A.Y -> 10; A.X -> 20 }

fun own(e: E, a: A): Int = when (a) { A.X -> 1; A.Y -> 2 } + when (e) { E.M -> 3; E.N -> 4 }

fun box(): String {
    val sum = caller(A.X, B.Q, E.N) + own(E.M, A.Y)
    return if (sum == 1 + 20 + 100 + 2 + 3) "OK" else "FAIL $sum"
}
