// An `@OptIn` on an accessor, on a property's `@get:`/`@set:` use-site, or on the trailing value
// statement of a lambda covers the opted-in uses inside that element only.

@RequiresOptIn
annotation class Internal

class Store {
    @Internal
    val name: String = "x"
}

fun store() = Store()

class View {
    val viaGetter: String
        @OptIn(Internal::class)
        get() = store().name

    @get:OptIn(Internal::class)
    val viaGetSite: String
        get() = store().name

    var viaSetter: String = ""
        @OptIn(Internal::class)
        set(value) {
            field = store().name + value
        }

    @set:OptIn(Internal::class)
    var viaSetSite: String = ""
        set(value) {
            field = value + store().name
        }

    val viaBlockGetter: String
        @OptIn(Internal::class)
        get() {
            return store().name
        }
}

val topLevel: String
    @OptIn(Internal::class)
    get() = store().name

fun trailingLambdaValue(): String = run {
    val prefix = "y"
    @OptIn(Internal::class)
    prefix + store().name
}

fun box(): String {
    val view = View()
    view.viaSetter = "a"
    view.viaSetSite = "b"
    val all = view.viaGetter + view.viaGetSite + view.viaSetter + view.viaSetSite +
        view.viaBlockGetter + topLevel + trailingLambdaValue()
    return if (all == "xxxabxxxyx") "OK" else "fail: $all"
}
