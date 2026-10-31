open class B {
    protected fun foo(): String = "member"
    private fun bar(): String = "member"
    protected operator fun plus(o: B): String = "member"
}
class X : B()
fun B.foo(): String = "O"
fun B.bar(): String = "K"
operator fun B.plus(o: B): String = ""
class Col(val name: String) {
    private fun tag(): String = "member"
}
fun Col.tag(): String = name
fun direct(c: Col): String = c.tag()
fun safe(b: B?): String? = b?.foo()
fun implicit(b: B): String = with(b) { bar() }
fun box(): String = (safe(X()) ?: "fail") + implicit(X()) + (X() + X()) + direct(Col(""))
