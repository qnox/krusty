// Only `main(args: Array<String>)` is an entry point; the overloads beside it are ordinary
// functions the entry may call.
fun main(count: Int) {
    println("count " + count)
}

fun main(args: Array<String>, extra: Int) {
    println("extra " + extra)
}

fun main(args: Array<String>) {
    println("entry " + args.size)
    main(args.size)
    main(args, 7)
}
