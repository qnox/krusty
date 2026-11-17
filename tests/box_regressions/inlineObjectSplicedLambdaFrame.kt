// A crossinline lambda an inline function's anonymous object invokes is spliced into each call's
// copy of the object, inside its own inline-lambda frame.
inline fun wrap(crossinline text: () -> String) =
    object {
        override fun toString() = text()
    }

fun box(): String = wrap { "O" }.toString() + wrap { "K" }.toString()
