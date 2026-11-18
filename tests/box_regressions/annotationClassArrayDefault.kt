// WITH_STDLIB
import kotlin.reflect.KClass

annotation class Application(
    val configurations: Array<String> = [],
    val modules: Array<KClass<*>> = [Unit::class],
)

@Application
class Annotated

fun box(): String = "OK"
