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

class Packed(vararg val words: String)

class Numbers(vararg val ids: Int)

class NativeApi {
    external fun ping(): Int
}

abstract class Fields(val fromConstructor: Int) {
    val initialized: Int = 1
    val computed: Int get() = 2
    var usesField: Int = 3
        get() = field + 1
    val readsField: Int
        get() = field
    val blockReadsField: Int
        get() {
            return field
        }
    lateinit var late: String
    val delegated: Int by lazy { 4 }
    abstract val pending: Int
    var noField: Int
        get() = 5
        set(value) {}
    val shadowed: Int
        get() {
            val field = 8
            return field
        }
    val throughLambda: Int
        get() = run { field }
    val throughLocalFunction: Int
        get() {
            fun read() = field
            return read()
        }
    val lambdaParameter: Int
        get() = 9.let { field -> field }
    val localFunctionParameter: Int
        get() {
            fun read(field: Int) = field
            return read(10)
        }
    val throughObject: Int
        get() = object {
            fun read() = field
        }.read()
    var setterShadowed: Int
        get() = 11
        set(value) {
            val field = value
            println(field)
        }

    init {
        readsField = 6
        blockReadsField = 7
        throughLambda = 13
        throughLocalFunction = 14
        throughObject = 15
    }
}
