// MODULE: lib
interface ExternalFn : (Int) -> Int
// MODULE: main(lib)
interface LocalFn : ExternalFn
class Impl : ExternalFn { override fun invoke(value: Int) = value }
class LocalImpl : LocalFn { override fun invoke(value: Int) = value }
fun box(): String {
    val direct: Any = Impl()
    val throughLocal: Any = LocalImpl()
    if (direct !is Function<*>) return "direct common"
    if (direct !is Function1<*, *>) return "direct arity"
    if (throughLocal !is Function<*>) return "local common"
    if (throughLocal !is Function1<*, *>) return "local arity"
    return "OK"
}
