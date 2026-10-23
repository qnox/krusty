abstract class AbstractSizedValues : Collection<Int> {
    abstract override val size: Int
}

interface SizedInterface : Collection<Int> {
    override val size: Int
}

fun box(): String = "OK"
