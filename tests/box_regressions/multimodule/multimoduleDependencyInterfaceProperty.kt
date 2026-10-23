// MODULE: lib
interface Named {
    val name: String
}

class LibNamed : Named {
    override val name: String get() = "lib"
}

fun nameOf(named: Named): String = named.name

fun nameOfLib(): String = nameOf(LibNamed())

// MODULE: main(lib)
class MainNamed : Named {
    override val name: String get() = "main"
}

fun box(): String {
    val names = nameOfLib() + " " + nameOf(MainNamed())
    return if (names == "lib main") "OK" else "fail: $names"
}
