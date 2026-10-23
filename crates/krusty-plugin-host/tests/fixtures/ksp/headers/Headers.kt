package headers

import java.io.Serializable

interface Named

abstract class Base<T : Any, in I> : Named, Serializable

open class Middle<out E : CharSequence?> : Base<List<E>, String>(), Comparable<Middle<*>> {
    override fun compareTo(other: Middle<*>): Int = 0
}

sealed interface Shape<S> : Named where S : Number, S : Comparable<S>

private class Hidden : Shape<Int>

internal annotation class Marker

enum class Color { RED }

class Outer {
    inner class In<X> : Named, Comparable<X?> {
        override fun compareTo(other: X?): Int = 0
    }

    data object Single
}

fun interface Action : () -> Unit
