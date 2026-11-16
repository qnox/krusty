// `import` is a soft keyword: it introduces a directive only in a file's import list and is an
// ordinary name everywhere else, as a declaration, parameter, loop variable, lambda parameter,
// named argument, and callable reference.
import kotlin.math.abs

class Directive(val import: String)

class Importer(private val prefix: String) {
    fun import(suffix: String): String = prefix + suffix
}

val import = "O"

fun describe(import: Directive) = import.import

fun named(import: String) = import

fun box(): String {
    val directive = Directive("O")
    var collected = ""
    for (import in listOf("O", "K")) {
        collected += import
    }
    val lambda = { import: String -> import }
    val reference = Directive::import
    val result = listOf(
        describe(directive) + named(import = "K"),
        collected,
        lambda("O") + Importer("").import("K"),
        reference.get(Directive("OK")),
        import + abs(-1).toString().replace("1", "K"),
    )
    return if (result.all { it == "OK" }) "OK" else "FAIL: $result"
}
