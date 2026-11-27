// MODULE: lib
// A library function reaches an interface default only through a `super` call that sits in an
// anonymous class it constructs, or in the initializer of a top-level property it reads. Each
// class overrides the default, and nothing else calls it, so the consumer's unit has to emit the
// default for the `super` call to run.
interface Greeter {
    fun greet(): String = "hi"
}

interface Namer {
    fun name(): String = "n"
}

interface Shouter {
    fun shout(): String
}

val initializerShouter: Shouter = object : Namer, Shouter {
    override fun name(): String = "overridden"
    override fun shout(): String = super<Namer>.name() + "?"
}

fun anonymousGreeting(): String {
    val greeter = object : Greeter {
        override fun greet(): String = "overridden"
        fun shout(): String = super<Greeter>.greet() + "!"
    }
    return greeter.shout()
}

fun fileInitializerName(): String = initializerShouter.shout()

// MODULE: main(lib)
fun box(): String {
    if (anonymousGreeting() != "hi!") return "anonymous: ${anonymousGreeting()}"
    if (fileInitializerName() != "n?") return "file initializer: ${fileInitializerName()}"
    return "OK"
}
