// A member property delegated to Delegates.notNull, written in the constructor and read afterwards.
class My {
    var delegate: String by kotlin.properties.Delegates.notNull()
        private set
    init {
        delegate = "OK"
    }
}
fun box(): String = My().delegate
