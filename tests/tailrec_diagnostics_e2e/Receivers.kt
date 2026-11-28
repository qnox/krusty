open class Base { open fun f(n: Int, a: Int = 0): Int = n }
class OtherInstance {
    tailrec fun onParameter(n: Int, o: OtherInstance): Int = if (n == 0) 0 else o.onParameter(n - 1, o)
    tailrec fun onNewInstance(n: Int): Int = if (n == 0) 0 else OtherInstance().onNewInstance(n - 1)
    tailrec fun inReceiverLambda(n: Int, o: OtherInstance): Int = if (n == 0) 0 else with(o) { inReceiverLambda(n - 1, o) }
    tailrec fun delegated(n: Int): Int = if (n == 0) 0 else Delegate().call(n)
    class Delegate { fun call(n: Int) = n }
}
open class PrivateMember { private tailrec fun pm(n: Int, o: PrivateMember): Int = if (n == 0) 0 else o.pm(n - 1, o) }
class OverridesWithDefault : Base() {
    override tailrec fun f(n: Int, a: Int): Int = if (n == 0) a else f(n - 1, a)
}
class OverridesOmittingArgument : Base() {
    override tailrec fun f(n: Int, a: Int): Int = if (n == 0) a else this.f(n - 1)
}
