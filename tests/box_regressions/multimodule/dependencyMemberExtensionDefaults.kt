// MODULE: lib
package lib

class Path(val text: String)

object Paths {
    var prefix = "/"

    // An omitted default reads the dispatch receiver's state, so the call must pass the object
    // through the dependency's default bridge.
    fun String.toPath(normalize: Boolean = false, root: String = prefix): Path =
        Path(if (normalize) root + this else this)
}

object Units {
    var scale = 10

    fun Int.scaled(by: Int = scale): Int = this * by
}

// MODULE: main(lib)
import lib.Paths
import lib.Paths.toPath
import lib.Units
import lib.Units.scaled

fun box(): String {
    if ("a".toPath().text != "a") return "fail no defaults supplied"
    if ("b".toPath(true).text != "/b") return "fail second default"
    if ("c".toPath(true, "~").text != "~c") return "fail all supplied"
    Paths.prefix = "#"
    if ("d".toPath(normalize = true).text != "#d") return "fail default after state change"
    if (4.scaled() != 40) return "fail object default"
    Units.scale = 3
    if (4.scaled() != 12) return "fail object default after state change"
    if (4.scaled(2) != 8) return "fail object supplied"
    return "OK"
}
