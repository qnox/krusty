//! End-to-end "box" coverage suite (t): exercises less-common lowering/emit/resolve branches not
//! covered by earlier suites — compound assignment across scalar/array/collection lvalues, augmented
//! operator overloads, inc/dec on properties and array elements, Long bit ops, value-returning `when`
//! (with call subjects and guards), assorted `for`/`while`/`do-while` forms, string/char helpers,
//! smart casts, elvis/safe-call chains, nullable arithmetic, the require/check/error/TODO intrinsics,
//! and labeled returns from nested lambdas. Each test compiles a `fun box(): String` returning "OK"
//! and round-trips it on the JVM.

mod common;

fn run(src: &str, stem: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    common::compile_and_run_box(src, stem, &[sl], Some(&jdk))
}

#[test]
fn compound_assign_scalars() {
    const SRC: &str = "fun box(): String {\n\
    var i = 10\n\
    i += 5; i -= 3; i *= 2; i /= 4; i %= 5\n\
    if (i != 1) return \"i=$i\"\n\
    var l = 100L\n\
    l += 50L; l *= 2L; l -= 25L; l /= 5L; l %= 7L\n\
    if (l != 6L) return \"l=$l\"\n\
    var d = 10.0\n\
    d += 2.5; d -= 0.5; d *= 2.0; d /= 4.0\n\
    if (d != 6.0) return \"d=$d\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "CompoundScalars") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn compound_assign_array_elements() {
    const SRC: &str = "fun box(): String {\n\
    val a = intArrayOf(1, 2, 3, 4)\n\
    a[0] += 10\n\
    a[1] *= 3\n\
    a[2] -= 1\n\
    a[3] %= 3\n\
    if (a[0] != 11) return \"a0=${a[0]}\"\n\
    if (a[1] != 6) return \"a1=${a[1]}\"\n\
    if (a[2] != 2) return \"a2=${a[2]}\"\n\
    if (a[3] != 1) return \"a3=${a[3]}\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "CompoundArray") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn compound_assign_map_and_list() {
    const SRC: &str = "fun box(): String {\n\
    val m = hashMapOf(\"x\" to 1, \"y\" to 2)\n\
    m[\"x\"] = m[\"x\"]!! + 5\n\
    if (m[\"x\"] != 6) return \"mx=${m[\"x\"]}\"\n\
    val list = mutableListOf(10, 20, 30)\n\
    list[1] += 100\n\
    if (list[1] != 120) return \"l1=${list[1]}\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "CompoundMapList") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn plus_assign_operator_overload() {
    const SRC: &str = "class Acc(var total: Int) {\n\
    operator fun plusAssign(x: Int) { total += x }\n\
}\n\
fun box(): String {\n\
    val a = Acc(0)\n\
    a += 3\n\
    a += 4\n\
    if (a.total != 7) return \"total=${a.total}\"\n\
    val bag = mutableListOf(1, 2)\n\
    bag += 3\n\
    bag += listOf(4, 5)\n\
    if (bag.sum() != 15) return \"sum=${bag.sum()}\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "PlusAssignOp") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn inc_dec_property_and_array() {
    const SRC: &str = "class Counter(var n: Int)\n\
fun box(): String {\n\
    val c = Counter(5)\n\
    c.n++\n\
    ++c.n\n\
    c.n--\n\
    if (c.n != 6) return \"n=${c.n}\"\n\
    val a = intArrayOf(0, 0, 0)\n\
    a[0]++\n\
    a[1]--\n\
    ++a[2]\n\
    if (a[0] != 1 || a[1] != -1 || a[2] != 1) return \"a=${a[0]},${a[1]},${a[2]}\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "IncDecPropArr") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn long_bit_ops_and_shifts() {
    const SRC: &str = "fun box(): String {\n\
    val x = 0xF0L\n\
    val y = 0x0FL\n\
    if ((x or y) != 0xFFL) return \"or\"\n\
    if ((x and 0x30L) != 0x30L) return \"and\"\n\
    if ((x xor 0xFFL) != 0x0FL) return \"xor\"\n\
    if (x.inv() != -0xF1L) return \"inv=${x.inv()}\"\n\
    if ((1L shl 10) != 1024L) return \"shl\"\n\
    if ((1024L shr 2) != 256L) return \"shr\"\n\
    if ((-1L ushr 60) != 15L) return \"ushr=${-1L ushr 60}\"\n\
    val mask = (0xFFL shl 8) and 0xF000L\n\
    if (mask != 0xF000L) return \"mask=$mask\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "LongBitOps") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn when_value_with_call_subject_and_guard() {
    const SRC: &str = "fun classify(n: Int): String = when (n % 3) {\n\
    0 -> \"zero\"\n\
    1 -> \"one\"\n\
    else -> \"two\"\n\
}\n\
fun box(): String {\n\
    if (classify(9) != \"zero\") return \"c9\"\n\
    if (classify(7) != \"one\") return \"c7\"\n\
    if (classify(8) != \"two\") return \"c8\"\n\
    val x = 5\n\
    val y = 3\n\
    val r = when {\n\
        x > 0 && y > 0 -> \"both\"\n\
        x > 0 -> \"x\"\n\
        else -> \"none\"\n\
    }\n\
    if (r != \"both\") return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "WhenValueGuard") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn nested_when_returning_values() {
    const SRC: &str = "fun grade(a: Int, b: Int): String = when (a) {\n\
    1 -> when (b) {\n\
        1 -> \"11\"\n\
        else -> \"1x\"\n\
    }\n\
    else -> when (b) {\n\
        1 -> \"x1\"\n\
        else -> \"xx\"\n\
    }\n\
}\n\
fun box(): String {\n\
    if (grade(1, 1) != \"11\") return \"g11\"\n\
    if (grade(1, 2) != \"1x\") return \"g1x\"\n\
    if (grade(2, 1) != \"x1\") return \"gx1\"\n\
    if (grade(2, 2) != \"xx\") return \"gxx\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "NestedWhen") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn for_until_step_and_withindex() {
    const SRC: &str = "fun box(): String {\n\
    var sum = 0\n\
    for (i in 0 until 10 step 2) sum += i\n\
    if (sum != 20) return \"sum=$sum\"\n\
    val list = listOf(\"a\", \"b\", \"c\")\n\
    val sb = StringBuilder()\n\
    for ((i, v) in list.withIndex()) sb.append(\"$i$v\")\n\
    if (sb.toString() != \"0a1b2c\") return \"sb=$sb\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "ForUntilStep") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn for_over_chars_and_reversed_range() {
    const SRC: &str = "fun box(): String {\n\
    var count = 0\n\
    for (c in \"hello\") if (c == 'l') count++\n\
    if (count != 2) return \"count=$count\"\n\
    var down = \"\"\n\
    for (i in 3 downTo 1) down += i.toString()\n\
    if (down != \"321\") return \"down=$down\"\n\
    var rev = 0\n\
    for (i in 10 downTo 0 step 3) rev += i\n\
    if (rev != 22) return \"rev=$rev\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "ForCharsReversed") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn while_and_do_while_break_continue() {
    const SRC: &str = "fun box(): String {\n\
    var i = 0\n\
    var sum = 0\n\
    while (i < 100 && sum < 20) {\n\
        i++\n\
        if (i % 2 == 0) continue\n\
        sum += i\n\
        if (sum > 15) break\n\
    }\n\
    if (sum != 16) return \"sum=$sum\"\n\
    var n = 0\n\
    var product = 1\n\
    do {\n\
        n++\n\
        product *= n\n\
    } while (n < 4)\n\
    if (product != 24) return \"product=$product\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "WhileDoWhile") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn string_and_char_helpers() {
    const SRC: &str = "fun box(): String {\n\
    val chars = charArrayOf('a', 'b', 'c')\n\
    val s = String(chars)\n\
    if (s != \"abc\") return \"s=$s\"\n\
    val built = buildString {\n\
        append(\"x\")\n\
        append(1)\n\
        append('!')\n\
    }\n\
    if (built != \"x1!\") return \"built=$built\"\n\
    val arr: CharArray = \"hi\".toCharArray()\n\
    if (arr.size != 2 || arr[0] != 'h') return \"arr\"\n\
    if ('e' !in \"hello\") return \"contains\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "StringCharHelpers") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn smart_cast_through_when_and_is() {
    const SRC: &str = "fun describe(x: Any): String = when (x) {\n\
    is Int -> \"int:${x + 1}\"\n\
    is String -> \"str:${x.length}\"\n\
    is List<*> -> \"list:${x.size}\"\n\
    else -> \"other\"\n\
}\n\
fun box(): String {\n\
    if (describe(41) != \"int:42\") return \"d1\"\n\
    if (describe(\"abc\") != \"str:3\") return \"d2\"\n\
    if (describe(listOf(1, 2)) != \"list:2\") return \"d3\"\n\
    if (describe(3.0) != \"other\") return \"d4\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "SmartCastWhen") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn smart_cast_through_null_and_and() {
    const SRC: &str = "fun len(s: String?): Int {\n\
    if (s != null && s.length > 0) return s.length\n\
    return -1\n\
}\n\
fun box(): String {\n\
    if (len(\"hello\") != 5) return \"l1\"\n\
    if (len(\"\") != -1) return \"l2\"\n\
    if (len(null) != -1) return \"l3\"\n\
    val x: Any? = \"world\"\n\
    if (x is String && x.length == 5) return \"OK\"\n\
    return \"fail\"\n\
}\n";
    let Some(out) = run(SRC, "SmartCastNull") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn elvis_and_safe_call_chains() {
    const SRC: &str = "fun lookup(k: String): String? = if (k == \"a\") \"alpha\" else null\n\
fun box(): String {\n\
    var side = 0\n\
    val r = lookup(\"z\") ?: run { side = 1; \"default\" }\n\
    if (r != \"default\" || side != 1) return \"r=$r side=$side\"\n\
    val n: String? = \"hi\"\n\
    val chained = n?.also { }?.let { it.length }\n\
    if (chained != 2) return \"chained=$chained\"\n\
    val absent: String? = null\n\
    val c2 = absent?.also { }?.let { it.length }\n\
    if (c2 != null) return \"c2=$c2\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "ElvisSafeCall") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn nullable_arithmetic_and_bang_bang() {
    const SRC: &str = "fun box(): String {\n\
    val a: Int? = 3\n\
    val b: Int? = 4\n\
    val sum = a?.plus(b ?: 0)\n\
    if (sum != 7) return \"sum=$sum\"\n\
    val c: Int? = null\n\
    val s2 = c?.plus(1)\n\
    if (s2 != null) return \"s2=$s2\"\n\
    val forced: Int = a!! + b!!\n\
    if (forced != 7) return \"forced=$forced\"\n\
    if ((a < b)) return \"OK\"\n\
    return \"cmp\"\n\
}\n";
    let Some(out) = run(SRC, "NullableArith") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn require_check_intrinsics() {
    const SRC: &str = "fun safe(n: Int): Int {\n\
    require(n >= 0) { \"neg\" }\n\
    check(n < 100)\n\
    return n * 2\n\
}\n\
fun box(): String {\n\
    if (safe(5) != 10) return \"s\"\n\
    try {\n\
        safe(-1)\n\
        return \"no throw\"\n\
    } catch (e: IllegalArgumentException) {\n\
    }\n\
    try {\n\
        check(false) { \"bad state\" }\n\
        return \"no check throw\"\n\
    } catch (e: IllegalStateException) {\n\
    }\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "RequireCheck") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn error_and_todo_intrinsics() {
    const SRC: &str = "fun boom(): Int = error(\"boom\")\n\
fun notdone(): Int = TODO(\"later\")\n\
fun box(): String {\n\
    try {\n\
        boom()\n\
        return \"no error\"\n\
    } catch (e: IllegalStateException) {\n\
        if (e.message != \"boom\") return \"msg=${e.message}\"\n\
    }\n\
    try {\n\
        notdone()\n\
        return \"no todo\"\n\
    } catch (e: NotImplementedError) {\n\
    }\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "ErrorTodo") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn labeled_return_from_nested_lambda() {
    const SRC: &str = "fun firstEven(list: List<Int>): Int {\n\
    list.forEach { x ->\n\
        if (x % 2 == 0) return x\n\
    }\n\
    return -1\n\
}\n\
fun box(): String {\n\
    if (firstEven(listOf(1, 3, 4, 5)) != 4) return \"fe\"\n\
    val sum = listOf(1, 2, 3, 4).sumOf sums@{ n ->\n\
        if (n == 3) return@sums 0\n\
        n\n\
    }\n\
    if (sum != 7) return \"sum=$sum\"\n\
    val r = run label@{\n\
        listOf(1, 2, 3).forEach inner@{\n\
            if (it == 2) return@inner\n\
            if (it == 3) return@label \"three\"\n\
        }\n\
        \"end\"\n\
    }\n\
    if (r != \"three\") return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "LabeledReturn") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}
