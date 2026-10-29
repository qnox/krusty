package aliases

@Target(AnnotationTarget.TYPE)
annotation class Kept

typealias Plain = String
typealias KeptString = @Kept String
typealias MaybeText = String?
typealias Names = List<String>
typealias Callback = (Int) -> String

class Aliases(
    val plain: Plain,
    val nullablePlain: Plain?,
    val kept: KeptString,
    val maybe: MaybeText,
    val names: Names,
    val callback: Callback,
    val nested: List<KeptString>,
    val mapped: Map<Plain, Names>,
) {
    fun convert(input: Plain, callback: Callback): Names = listOf(callback(input.length))
}
