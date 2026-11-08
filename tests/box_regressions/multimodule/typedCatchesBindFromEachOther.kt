// MODULE: lib
// FILE: Lib.kt
class Pack<T>(val value: T?)
fun <T> empty(): Pack<T> = Pack(null)
fun parse(fail: Boolean): Pack<String> {
if (fail) throw IllegalStateException()
return Pack("a")
}
fun typedCatches(fail: Boolean): String {
val parsed = try {
parse(fail)
} catch (e: IllegalStateException) {
empty()
} catch (t: Throwable) {
empty()
}
return parsed.value ?: "none"
}
// MODULE: main(lib)
// FILE: Main.kt
fun box(): String = if (typedCatches(false) == "a" && typedCatches(true) == "none") "OK" else "FAIL"
