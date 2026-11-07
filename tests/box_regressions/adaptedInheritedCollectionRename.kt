// WITH_STDLIB
// FILE: AdaptedInheritedRename.kt
@JvmInline value class BridgeToken(val value: Int)
abstract class TokenLeaf : TokenChild() {
    override fun removeAt(index: Int): BridgeToken = BridgeToken(index + 2)
}
abstract class TokenChild : TokenParent() {
    override fun removeAt(index: Int): BridgeToken = BridgeToken(index + 1)
}
abstract class TokenParent : MutableList<BridgeToken> {
    override fun removeAt(index: Int): BridgeToken = BridgeToken(index)
}
fun box(): String = "OK"
