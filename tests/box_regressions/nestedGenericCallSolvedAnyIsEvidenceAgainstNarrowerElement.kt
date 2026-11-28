// A nested call whose input solves its variable to `Any` (`listOf(x as Any)`) keeps that solution
// as evidence: it cannot complete to `List<String>`, so only the `Any` overload is applicable.
fun pick(xs: List<String>): String = "strings"
fun pick(xs: Any): String = "any"

@Suppress("USELESS_CAST")
fun pickCast(x: Any): String = pick(listOf(x as Any))

fun box(): String {
    val picked = pickCast("s")
    return if (picked == "any") "OK" else "FAIL: $picked"
}
