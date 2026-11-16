// An enum `when` that only a top-level function's `$default` wrapper evaluates. `box` passes both
// arguments, so no call site copies the default; the wrapper still owns its `$WhenMappings` table.
enum class E { A, B }

fun pick(e: E, s: String = when (e) {
    E.A -> "OK"
    E.B -> "FAIL"
}): String = s

fun box(): String = pick(E.A, "OK")
