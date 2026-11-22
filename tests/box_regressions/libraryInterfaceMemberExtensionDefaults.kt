// A library interface's member-extension default, called from the interface's own default and
// from consumers through the interface: a class that inherits it runs the library body, one that
// overrides it runs its override, as the receiver's class decides.
// MODULE: lib
interface Words {
    fun String.word(): String = "base:" + this
    fun run(): String = "x".word()
}

fun runThrough(words: Words): String = words.run()

// MODULE: main(lib)
class Inheriting : Words

class Overriding : Words {
    override fun String.word(): String = "override:" + this
}

fun words(index: Int): Words = if (index == 0) Inheriting() else Overriding()

fun Words.wordOf(value: String): String = value.word()

fun box(): String {
    val inheriting = words(0)
    val overriding = words(1)
    if (inheriting.run() != "base:x") return "fail: inherited run"
    if (overriding.run() != "override:x") return "fail: overriding run"
    if (runThrough(inheriting) != "base:x") return "fail: library inherited run"
    if (runThrough(overriding) != "override:x") return "fail: library overriding run"
    if (inheriting.wordOf("y") != "base:y") return "fail: inherited word"
    if (overriding.wordOf("y") != "override:y") return "fail: overriding word"
    return "OK"
}
