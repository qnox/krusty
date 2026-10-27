package javamodifiers

class Plain(val primary: Int, var mutablePrimary: String) {
    val body: Int = 1
    var mutableBody: Int = 2
    private val hidden: Int = 3
    internal val inside: Int = 4
    lateinit var late: String
    var privateSet: Int = 5
        private set
    fun member(): Int = 6
    private fun privateMember() {}
    internal fun internalMember() {}
    suspend fun suspending() {}
    inline fun inlined() {}

    companion object {
        const val CONSTANT = 7
        val companionProperty = 9
        fun companionMember() {}
    }

    class Nested
    inner class Inner
    private class PrivateNested
    protected class ProtectedNested
}

open class Open {
    open fun overridable() {}
    protected open fun protectedMember() {}
    open val openProperty: Int = 0
}

abstract class Abstract : Open() {
    abstract fun abstractMember()
    abstract val abstractProperty: Int
}

interface Contract {
    fun required()
    fun defaulted() {}
    val requiredValue: Int
    val defaultedValue: Int get() = 0
}

object Single {
    fun objectMember() {}
    val objectProperty = 10
}

data class Data(val value: Int)

enum class Color { RED, GREEN }

annotation class Marker

internal class InternalClass

private class PrivateClass

sealed class Sealed

fun topLevel() {}
private fun privateTopLevel() {}
internal fun internalTopLevel() {}
val topLevelProperty = 11
var topLevelVariable = 12
const val TOP_CONSTANT = 13
