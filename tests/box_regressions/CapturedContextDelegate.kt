// LANGUAGE: +ContextParameters
// WITH_STDLIB
import kotlin.reflect.KProperty

class CapturedDelegate
class DelegateCtx(val text: String) {
    operator fun CapturedDelegate.getValue(thisRef: Any?, property: KProperty<*>): String = text
}
context(ctx: DelegateCtx)
fun selectedDelegateContext(): DelegateCtx = ctx

fun runInDelegateContext(body: context(DelegateCtx) () -> String): String = body(DelegateCtx("O"))
fun box(): String = runInDelegateContext {
    with(selectedDelegateContext()) {
        val suffix = "K"
        class Reader {
            fun get(): String {
                val selected by CapturedDelegate()
                return selected + suffix
            }
        }
        val anonymous = object {
            fun get(): String {
                val selected by CapturedDelegate()
                return selected + suffix
            }
        }
        if (Reader().get() == "OK" && anonymous.get() == "OK") "OK" else "captured delegate"
    }
}
