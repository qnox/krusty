package members

import java.io.Serializable

interface Named {
    val name: String
    fun describe(prefix: String = ""): String
}

abstract class Base<T : Any>(protected val seed: T?) : Named, Serializable {
    abstract override val name: String
    open fun seedOr(fallback: T): T = seed ?: fallback
}

data class Item(override val name: String, var count: Int = 0) : Base<String>(null) {
    private val tags: MutableList<String> = mutableListOf()
    internal var weight: Double? = null

    constructor(name: String, tags: List<String>) : this(name) {
        this.tags.addAll(tags)
    }

    override fun describe(prefix: String): String = prefix + name

    fun <R> mapTags(transform: (String) -> R): List<R> = tags.map(transform)

    suspend fun load(vararg ids: Long): Map<Long, Item?> = emptyMap()

    companion object {
        const val LIMIT: Int = 10
        fun empty(): Item = Item("")
    }
}

fun topLevel(items: Array<out Item>, sink: MutableMap<in String, *>): Unit {}

val topValue: List<Set<Int>> = emptyList()

val String.shout: String get() = uppercase()

annotation class Mark

class Marked @Mark constructor(val v: Int)

data class Hidden private constructor(val v: Int)

@ConsistentCopyVisibility
data class Kept private constructor(val v: Int)
