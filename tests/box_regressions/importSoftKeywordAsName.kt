// `import` is a soft keyword: it introduces a directive only in a file's import list and is an
// ordinary name everywhere else, as a declaration, parameter, loop variable, lambda parameter,
// named argument, and callable reference.
package soft

import soft.Directive

class Directive(val import: String)

class Importer(private val prefix: String) {
    fun import(suffix: String): String = prefix + suffix
}

val import = "O"

fun describe(import: Directive) = import.import

fun named(import: String) = import

fun box(): String {
    var collected = ""
    for (import in 0..1) {
        collected += if (import == 0) "O" else "K"
    }
    val lambda = { import: String -> import }
    val reference = Importer::import
    val ok = describe(Directive("O")) + named(import = "K") == "OK"
        && collected == "OK"
        && lambda("O") + Importer("").import("K") == "OK"
        && reference(Importer("O"), "K") == "OK"
        && import == "O"
    return if (ok) "OK" else "FAIL"
}
