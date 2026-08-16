import kotlin.properties.Delegates

var observed by Delegates.observable(1) { _, old, new -> println("$old to $new") }
val lazyVal by lazy { 42 }
