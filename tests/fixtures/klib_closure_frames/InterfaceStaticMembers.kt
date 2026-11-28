// A class implementing an interface inherits its instance members only: the interface's
// companion-block function and property are static, so the class neither inherits nor
// implements them, even when nothing calls them. A library-private object reached only through
// the interface, as the standard library's `EmptySequence` implements `Sequence` beside the
// static `Sequence.of`.
package fixture

interface Source {
    fun item(): String

    fun label(): String = "source"

    companion {
        fun of(): Source = Empty

        val count: Int
            get() = 0
    }
}

private object Empty : Source {
    override fun item(): String = "none"
}

fun emptySource(): Source = Empty
