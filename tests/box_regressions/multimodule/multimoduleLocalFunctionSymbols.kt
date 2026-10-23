// MODULE: lib
fun fromLib(): String {
fun local() = "O"
return local()
}
// MODULE: main(lib)
fun box(): String {
fun local() = "K"
return fromLib() + local()
}
