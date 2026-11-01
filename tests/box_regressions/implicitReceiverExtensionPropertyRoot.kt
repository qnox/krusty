// A qualified spelling whose root names both a root package (`kotlin`) and an extension property
// that reads on an implicit receiver: the property wins at that receiver's tower level.
class Holder(val text: String)

val Holder.kotlin: String get() = text

fun Holder.viaExtensionReceiver(): Int = kotlin.length

fun viaLambdaReceiver(holder: Holder): Int = with(holder) { kotlin.length + 1 }

fun box(): String {
    val holder = Holder("ok")
    if (holder.viaExtensionReceiver() != 2) return "fail: extension receiver"
    if (viaLambdaReceiver(holder) != 3) return "fail: lambda receiver"
    return "OK"
}
