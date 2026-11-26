// MODULE: lib
// FILE: Readers.kt
package fixture

// An abstract library class inherits one member from several interfaces and declares none of
// them, so a body calling the member names the fake override, and no inherited declaration has
// a body. kotlinc selects the declaration whose result is a subtype of every other's, as the class
// sees each result: `Reader<List<E>>.read()` returns `List<E>?`, which is less specific than
// `Strict<E>.read()`'s `List<E>`, although it comes first and names the same classifier.
interface Reader<T> {
    fun read(): T?
}

interface Strict<E> {
    fun read(): List<E>
}

abstract class Both<E> : Reader<List<E>>, Strict<E> {
    fun count(): Int = read().size
}

class Fixed(private val items: List<String>) : Both<String>() {
    override fun read(): List<String> = items
}

// A covariant result: `Narrow.make()`'s `String` refines `Wide.make()`'s `CharSequence`.
interface Wide {
    fun make(): CharSequence
}

interface Narrow {
    fun make(): String
}

abstract class Pick : Wide, Narrow {
    fun length(): Int = make().length
}

class Word(private val word: String) : Pick() {
    override fun make(): String = word
}

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val fixed = Fixed(listOf("a", "b", "c"))
    if (fixed.count() != 3) return "count ${fixed.count()}"
    val word = Word("four")
    if (word.length() != 4) return "length ${word.length()}"
    return "OK"
}
