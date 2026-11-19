// LANGUAGE: +MultiPlatformProjects
// An `expect` class's members are implemented by whatever the actual class's whole member scope
// holds, members its dependency supertypes declare included: an interface a library module
// declares, implemented through delegation or by an inherited implementation.

// MODULE: lib-common
interface Named {
    val name: String
    fun greet(greeting: String, punctuation: String?): String
    fun shout(word: String?): String
}

open class Base {
    open fun describe(width: Int): String = "base$width"
}

object English : Named {
    override val name: String get() = "en"
    override fun greet(greeting: String, punctuation: String?): String = greeting + (punctuation ?: "")
    override fun shout(word: String?): String = (word ?: "nothing") + "!"
}

// MODULE: lib-platform()()(lib-common)

// MODULE: app-common(lib-common)
// kotlinc-native reports an actual class whose delegated members carry a default value, so the
// library interface declares none.
expect class Speaker : Named {
    override val name: String
    override fun shout(word: String?): String
}

expect class Plain() : Base {
    override fun describe(width: Int): String
}

// MODULE: app-platform(lib-platform)()(app-common)
actual class Speaker : Named by English

actual class Plain actual constructor() : Base()

fun box(): String {
    val speaker = Speaker()
    if (speaker.name != "en") return "Fail name: ${speaker.name}"
    if (speaker.greet("hi", "!") != "hi!") return "Fail greet: ${speaker.greet("hi", "!")}"
    if (speaker.greet("hi", null) != "hi") return "Fail null"
    if (speaker.shout("hey") != "hey!") return "Fail shout: ${speaker.shout("hey")}"
    if (speaker.shout(null) != "nothing!") return "Fail shout null"
    if (Plain().describe(3) != "base3") return "Fail inherited: ${Plain().describe(3)}"
    return "OK"
}
