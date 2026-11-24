// A functional interface whose default redeclares a member of a generic interface at another
// carrier is called through the ancestor, its erased signature bridging to the default.
interface Parent<T> {
    fun value(): T
    fun echo(value: T): T
}

fun interface Child : Parent<Int> {
    override fun value(): Int = 42
    override fun echo(value: Int): Int = value + 1
    fun run(): String
}

fun box(): String {
    val child = Child { "OK" }
    val parent: Parent<Int> = child
    if (parent.value() != 42) return "Fail parent value: ${parent.value()}"
    if (child.value() != 42) return "Fail child value: ${child.value()}"
    if (parent.echo(4) != 5) return "Fail parent echo: ${parent.echo(4)}"
    if (child.echo(4) != 5) return "Fail child echo: ${child.echo(4)}"
    return child.run()
}
