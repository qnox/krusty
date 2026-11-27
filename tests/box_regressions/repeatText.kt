fun box(): String {
    if ("ab".repeat(3) != "ababab") return "FAIL: repeat"
    if ("x".repeat(0) != "") return "FAIL: repeat zero"
    if ('z'.toString().repeat(2) != "zz") return "FAIL: single character repeat"
    return "OK"
}
