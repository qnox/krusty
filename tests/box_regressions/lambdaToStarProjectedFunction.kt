// A lambda written for a projected function classifier takes its shape from the projection: a star
// parameter accepts nothing (`Function1<*, *>` gives `x` the type `Nothing`), an `in` parameter
// accepts its bound, and a star result reads as its upper bound.
fun box(): String {
    val star: Function1<*, *> = { x -> x.toString() }
    val consumer: Function1<in String, *> = { x -> x.length }
    val producer: Function1<*, String> = { _ -> "K" }
    val read = consumer("abc")
    return if (star is Function1<*, *> && producer is Function1<*, *> && read == 3) "OK" else "fail"
}
