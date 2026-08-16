package examples.formatting

enum class Color { RED, GREEN, BLUE }
enum class Severity { INFO, WARNING, ERROR; fun isFatal()=this==ERROR }
