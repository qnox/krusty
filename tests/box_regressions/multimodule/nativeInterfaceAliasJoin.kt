// MODULE: library
// FILE: library.kt
package interfacealias

interface Word {
    fun text(): String
    fun size(): Int
}

interface NamedWord : Word {
    override fun text(): String
}

class WordImpl : NamedWord {
    override fun text(): String = "OK"
    override fun size(): Int = 2
}

fun make(): Word = WordImpl()
fun makeNamed(): NamedWord = WordImpl()

// MODULE: main(library)
// FILE: text.kt
import interfacealias.Word

fun firstText(word: Word): String = word.text()

// FILE: textAgain.kt
import interfacealias.Word

fun secondText(word: Word): String = word.text()

// FILE: size.kt
import interfacealias.Word

fun wordSize(word: Word): Int = word.size()

// FILE: namedText.kt
import interfacealias.NamedWord

fun namedText(word: NamedWord): String = word.text()

// FILE: main.kt
import interfacealias.make
import interfacealias.makeNamed

fun box(): String {
    val word = make()
    if (firstText(word) != "OK" || secondText(word) != "OK") return "text"
    if (wordSize(word) != 2) return "size"
    if (namedText(makeNamed()) != "OK") return "namedText"
    return "OK"
}
