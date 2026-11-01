package parameters

@Target(AnnotationTarget.VALUE_PARAMETER)
annotation class Visible(val text: String, val nested: Nested)

@Target(AnnotationTarget.VALUE_PARAMETER)
@Retention(AnnotationRetention.BINARY)
annotation class Invisible(val values: IntArray)

annotation class Nested(val number: Int)

class Subject {
    fun member(
        @Invisible([1, 2]) @Visible("member-first", Nested(3)) first: String,
        @Visible("member-second", Nested(4)) @Invisible([5]) second: String?
    ): String = first + second

    companion object {
        @JvmStatic
        fun forwarded(
            @Invisible([6]) @Visible("static-first", Nested(7)) first: String,
            @Visible("static-second", Nested(8)) @Invisible([9, 10]) second: String?
        ): String = first + second
    }
}

interface Contract {
    fun inherited(
        @Invisible([11]) @Visible("interface-first", Nested(12)) first: String,
        @Visible("interface-second", Nested(13)) @Invisible([14]) second: String?
    ): String = first + second
}
