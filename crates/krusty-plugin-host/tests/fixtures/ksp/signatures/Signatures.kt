package signatures

import java.io.Serializable

@JvmInline
value class Meters(val value: Int)

interface Shape {
    fun area(): Double
}

open class Holder<T : Comparable<T>>(val item: T, var count: Int = 0) : Serializable {
    val names: List<String> = emptyList()
    var nullable: String? = null
    lateinit var late: String
    val array: IntArray = IntArray(0)
    val matrix: Array<Array<String>> = emptyArray()

    constructor(item: T, label: String) : this(item, label.length)

    fun plain() {}
    fun boxed(value: Int?, chars: CharSequence): Long? = null
    fun <R> generic(value: R, items: Map<String, List<R>>): R = value
    fun bounded(item: T): T = item
    fun vararg(vararg values: String): Array<out String> = values
    fun lambda(block: (Int) -> String): () -> Unit = {}
    suspend fun suspending(value: String): Int = 0
    fun meters(value: Meters): Meters = value
    fun nothing(): Nothing = throw IllegalStateException()
    fun unit(value: Unit): Unit = value
    fun Int.extension(other: String): Boolean = false
    @JvmName("renamed")
    fun named(): Char = 'c'
    fun primitives(b: Byte, s: Short, f: Float, d: Double, z: Boolean, c: Char, j: Long) {}
    fun star(value: List<*>, out: MutableList<out Number>, input: Comparable<in String>) {}
    fun nullableMeters(value: Meters?): Meters? = value
    fun returnsMeters(): Meters = Meters(1)
    internal fun hidden(value: Int) {}
    internal val secret = 1
    internal var secretVar = 1
    val isOpen: Boolean = true
    var isClosed: Boolean = false
    val metersProperty: Meters = Meters(1)
    @get:JvmName("fetch")
    @set:JvmName("store")
    var fetched: String = ""
    private fun privateFunction() {}
    protected open fun protectedFunction() {}

    inner class Inner(val outer: Int)
    class Nested
    companion object {
        const val CONSTANT = 1
        @JvmStatic fun create(): Holder<String> = Holder("")
    }
}

enum class Color { RED }

object Single {
    val value = 1
}

fun topLevel(value: Holder<String>): Shape? = null

val topProperty: Map<Int, String> = emptyMap()

typealias Alias = Holder<String>
