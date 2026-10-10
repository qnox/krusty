// Prints each argument's UTF-16 code units in decimal, so a replacement character or a surrogate
// pair shows as the exact units the program received.
fun main(args: Array<String>) {
    println(args.size)
    for (argument in args) {
        print(argument.length)
        print(":")
        for (unit in argument) {
            print(" ")
            print(unit.code)
        }
        println()
    }
}
