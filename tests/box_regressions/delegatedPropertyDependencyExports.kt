// MODULE: lib
interface ExportedView {
    val fixed: Int
    var mutable: Int
}

class BackingView(override val fixed: Int, override var mutable: Int) : ExportedView
class DelegatedView(value: ExportedView) : ExportedView by value
fun libraryDelegatedView(): DelegatedView = DelegatedView(BackingView(9, 2))

// MODULE: main(lib)
fun box(): String {
    val value = libraryDelegatedView()
    if (value.fixed != 9) return "delegated val"
    if (value.mutable != 2) return "delegated getter"
    value.mutable = 7
    if (value.mutable != 7 || value.fixed != 9) return "delegated setter"
    return "OK"
}
