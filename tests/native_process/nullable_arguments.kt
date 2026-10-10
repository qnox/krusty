// The frontend accepts a nullable array as `main`'s parameter; the program receives the arguments.
fun main(args: Array<String>?) {
    println(args!!.size)
    for (argument in args) println(argument)
}
