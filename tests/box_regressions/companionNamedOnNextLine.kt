// A line break is whitespace in a declaration header, so a bare `companion object` takes a name
// written on the next line, as kotlinc does. A hard keyword there (`object Other`) is not a name: it
// starts the next declaration and leaves the companion named `Companion`.
class Named {
    companion object
    Factory {
        fun make() = "O"
    }
}

class Unnamed {
    companion object
    object Other {
        fun make() = "K"
    }
}

fun box(): String {
    val result = Named.Factory.make() + Unnamed.Other.make()
    return if (result == "OK") "OK" else "FAIL: $result"
}
