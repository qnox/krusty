// MODULE: lib
interface Named {
    val name: String
    var count: Int
    val greeting: String get() = "hi " + name
}

class LibNamed : Named {
    override val name: String get() = "lib"
    override var count: Int = 1
}

fun describe(named: Named): String {
    named.count = named.count + 1
    return named.greeting + named.count
}

fun describeLib(): String = describe(LibNamed())

// MODULE: main(lib)
class MainNamed : Named {
    override val name: String get() = "main"
    override var count: Int = 10
}

class MainGreeter : Named {
    override val name: String get() = "greeter"
    override var count: Int = 20
    override val greeting: String get() = "yo " + name
}

fun box(): String {
    val described = describeLib() + " " + describe(MainNamed()) + " " + describe(MainGreeter())
    return if (described == "hi lib2 hi main11 yo greeter21") "OK" else "fail: $described"
}
