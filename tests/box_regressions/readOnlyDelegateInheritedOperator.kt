import kotlin.properties.ReadOnlyProperty
import kotlin.reflect.KProperty
class Delegate : ReadOnlyProperty<Test, String> {
    override fun getValue(thisRef: Test, property: KProperty<*>) = "OK"
    fun getValue(thisRef: String, property: KProperty<*>) = "wrong"
}
class Test {
    val message by Delegate()
}
fun box(): String = Test().message
