// An inline function's default-argument expression that calls a file-private function runs in the
// caller's class, so it reaches the function through the facade's public `access$` accessor.

// FILE: defaults.kt
private fun filePrivateIdentity(value: Int): Int = value

internal inline fun inlineWithDefault(value: Int, fallback: Int = filePrivateIdentity(value)): Int =
    fallback

// FILE: main.kt
fun box(): String {
    if (inlineWithDefault(42) != 42) return "FAIL default"
    if (inlineWithDefault(42, 7) != 7) return "FAIL explicit"
    return "OK"
}
