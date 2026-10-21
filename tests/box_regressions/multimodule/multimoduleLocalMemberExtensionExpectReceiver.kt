// MODULE: common
expect class Receiver {
    val prefix: String
}
interface Left {
    fun read(receiver: Receiver): String
    val Receiver.suffix: String
}
interface Right {
    fun read(receiver: Receiver): String
    val Receiver.suffix: String
}
interface Both : Left, Right
fun exercise(receiver: Receiver): String {
    class Local : Both {
        override fun read(receiver: Receiver): String = receiver.prefix
        override val Receiver.suffix: String get() = "K"
    }
    val local = Local()
    return with(local) { local.read(receiver) + receiver.suffix }
}
// MODULE: main()()(common)
class ActualReceiver(val prefix: String)
actual typealias Receiver = ActualReceiver
fun box(): String = exercise(ActualReceiver("O"))
