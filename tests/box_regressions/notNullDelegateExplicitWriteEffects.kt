// An explicit operator call evaluates the runtime-unused `thisRef` before the property and value
// arguments. Generated delegated access supplies a side-effect-free receiver, so it would not
// expose this ordering rule.
import kotlin.properties.Delegates
var referenced = 0
var effects = ""
fun mark(): Any? { effects += "R"; return null }
fun next(): Int { effects += "V"; return 7 }
fun box(): String {
    val delegate = Delegates.notNull<Int>()
    delegate.setValue(mark(), ::referenced, next())
    if (effects != "RV") return "fail effects " + effects
    return if (delegate.getValue(null, ::referenced) == 7) "OK" else "fail value"
}
