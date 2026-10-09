// Prints each line standard input yields as its UTF-16 code units in decimal, a run of one unit
// written `unit*count`, then what is read at the end of input.
fun main() {
    while (true) {
        val line = readLine() ?: break
        print(line.length)
        print(":")
        var at = 0
        while (at < line.length) {
            var end = at + 1
            while (end < line.length && line[end] == line[at]) end++
            print(" ")
            print(line[at].code)
            if (end - at > 1) {
                print("*")
                print(end - at)
            }
            at = end
        }
        println()
    }
    println("end " + readlnOrNull())
}
