// MODULE: lib
// FILE: Library.kt
open class RenderInt : Function1<Int, String> {
    override fun invoke(value: Int): String = "value:$value"
}

fun renderInLibrary(value: Int): String {
    val render: (Int) -> String = RenderInt()
    return render(value)
}

// MODULE: main(lib)
// FILE: Main.kt
class InheritedRenderInt : RenderInt()

fun box(): String {
    val render: (Int) -> String = InheritedRenderInt()
    if (render(7) != "value:7") return "inherited function bridge"
    if (renderInLibrary(8) != "value:8") return "producer function bridge"
    val generated: Any = { value: Int -> "lambda:$value" }
    val lambda = generated as (Int) -> String
    if (lambda(9) != "lambda:9") return "generated lambda invoke"
    return "OK"
}
