//! kotlinc's `SwitchGenerator`: a `when` comparing one `String` local against three or more
//! distinct string hashes switches on `hashCode()` and resolves each bucket with `equals`, with
//! the buckets laid out in `HashMap` iteration order; a `Char` local switches on its code.
use super::common;

const SOURCE: &str = "package store\n\
    \n\
    fun four(x: String): String = when (x) {\n\
    \x20   \"abc\", \"cde\" -> \"abc_cde\"\n\
    \x20   \"efg\", \"ghi\" -> \"efg_ghi\"\n\
    \x20   else -> \"other\"\n\
    }\n\
    fun nullable(x: String?): String {\n\
    \x20   when (x) {\n\
    \x20       \"abc\", \"cde\" -> return \"abc_cde\"\n\
    \x20       \"efg\", \"ghi\", null -> return \"efg_ghi\"\n\
    \x20   }\n\
    \x20   return \"other\"\n\
    }\n\
    fun nullableElse(x: String?): Int = when (x) { \"a\" -> 1; \"b\" -> 2; \"c\" -> 3; else -> 4 }\n\
    fun collide(x: String): Int = when (x) { \"Aa\" -> 1; \"BB\" -> 2; \"C\" -> 3; \"Aa\" -> 4; \"D\" -> 5; else -> 6 }\n\
    fun two(x: String): Int = when (x) { \"a\" -> 1; \"b\" -> 2; else -> 3 }\n\
    fun many(x: String): Int = when (x) {\n\
    \x20   \"zero\" -> 0; \"one\" -> 1; \"two\" -> 2; \"three\" -> 3; \"four\" -> 4; \"five\" -> 5; \"six\" -> 6\n\
    \x20   \"seven\" -> 7; \"eight\" -> 8; \"nine\" -> 9; \"ten\" -> 10; \"eleven\" -> 11; \"twelve\" -> 12\n\
    \x20   \"thirteen\" -> 13\n\
    \x20   else -> -1\n\
    }\n\
    fun chars(c: Char): Int = when (c) { 'a' -> 1; 'b', 'c' -> 2; 'z' -> 3; else -> 4 }\n";

/// `HashMap` bins past the treeify threshold. A two-letter key `ab` hashes to `31 * a + b`: nine
/// hashes in one 16-slot bin (`2080 + 16k`) treeify it, which grows the table to 32 slots instead;
/// fourteen in one 64-slot bin (`2112 + 64k`) make a red-black tree bin that iterates root first;
/// and forty-nine keys grow a tree bin's table to 128 slots, re-treeifying its nine-node half and
/// untreeifying its six-node half.
const HASH_BINS_SOURCE: &str = "package store\n\
    fun treeifyResize(x: String): Int = when (x) {\n\
    \x20   \"AD\" -> 0; \"AA\" -> 1; \"AQ\" -> 2; \"Aa\" -> 3; \"Aq\" -> 4; \"Bb\" -> 5; \"AT\" -> 6; \"Br\" -> 7\n\
    \x20   \"Cc\" -> 8; \"Cs\" -> 9; \"Dd\" -> 10\n\
    \x20   else -> -1\n\
    }\n\
    fun treeBin(x: String): Int = when (x) {\n\
    \x20   \"Af\" -> 0; \"Aa\" -> 1; \"Cc\" -> 2; \"Ee\" -> 3; \"Gg\" -> 4; \"Ii\" -> 5; \"Kk\" -> 6; \"Mm\" -> 7\n\
    \x20   \"Oo\" -> 8; \"Qq\" -> 9; \"Ss\" -> 10; \"Uu\" -> 11; \"Ww\" -> 12; \"Yy\" -> 13; \"aA\" -> 14; \"AI\" -> 15\n\
    \x20   else -> -1\n\
    }\n\
    fun splitTreeBin(x: String): Int = when (x) {\n\
    \x20   \"Aa\" -> 0; \"Cc\" -> 1; \"Ee\" -> 2; \"Gg\" -> 3; \"Ii\" -> 4; \"Kk\" -> 5; \"Mm\" -> 6; \"Oo\" -> 7\n\
    \x20   \"Qq\" -> 8; \"Ss\" -> 9; \"Uu\" -> 10; \"Ww\" -> 11; \"aA\" -> 12; \"dd\" -> 13; \"hh\" -> 14; \"AH\" -> 15\n\
    \x20   \"Aj\" -> 16; \"Bl\" -> 17; \"Cn\" -> 18; \"Dp\" -> 19; \"Er\" -> 20; \"Ft\" -> 21; \"Gv\" -> 22; \"Hx\" -> 23\n\
    \x20   \"Iz\" -> 24; \"Ka\" -> 25; \"Lc\" -> 26; \"Me\" -> 27; \"Ng\" -> 28; \"Oi\" -> 29; \"Pk\" -> 30; \"Qm\" -> 31\n\
    \x20   \"Ro\" -> 32; \"Sq\" -> 33; \"Ts\" -> 34; \"Vw\" -> 35; \"Wy\" -> 36; \"ZA\" -> 37; \"Zc\" -> 38; \"aF\" -> 39\n\
    \x20   \"ah\" -> 40; \"bj\" -> 41; \"cl\" -> 42; \"dn\" -> 43; \"ep\" -> 44; \"fr\" -> 45; \"gt\" -> 46; \"hv\" -> 47\n\
    \x20   \"ix\" -> 48\n\
    \x20   else -> -1\n\
    }\n";

/// A platform `String!` subject may be `null`, so kotlinc tests it with `ifnull` before
/// `hashCode()`: in a plain function, in a SAM lambda, and with an explicit `null` case.
const PLATFORM_SOURCE: &str = "package store\n\
    import java.lang.reflect.InvocationHandler\n\
    import java.lang.reflect.Method\n\
    fun member(method: Method): String = when (method.name) {\n\
    \x20   \"toString\" -> \"a\"; \"hashCode\" -> \"b\"; \"equals\" -> \"c\"; else -> \"d\"\n\
    }\n\
    fun handler(): InvocationHandler = InvocationHandler { _, method, _ ->\n\
    \x20   when (method.name) {\n\
    \x20       \"toString\" -> \"proxy\"; \"hashCode\" -> 7; \"equals\" -> false; else -> null\n\
    \x20   }\n\
    }\n\
    fun property(key: String): Int = when (System.getProperty(key)) {\n\
    \x20   \"a\" -> 1; \"b\" -> 2; null -> 3; else -> 4\n\
    }\n\
    fun declared(method: Method): Int {\n\
    \x20   val name: String = method.name\n\
    \x20   return when (name) { \"a\" -> 1; \"b\" -> 2; \"c\" -> 3; else -> 4 }\n\
    }\n";

fn assert_identical(stem: &str, source: &str, class: &str) {
    common::byte_diff_against_kotlinc_cp(stem, source, class, &[common::stdlib_jar()])
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn string_and_char_whens_switch_like_kotlinc() {
    assert_identical("StringWhenSwitch", SOURCE, "store/StringWhenSwitchKt");
}

#[test]
fn string_hash_buckets_follow_hash_map_tree_bins_like_kotlinc() {
    assert_identical(
        "StringWhenHashBins",
        HASH_BINS_SOURCE,
        "store/StringWhenHashBinsKt",
    );
}

#[test]
fn platform_string_switches_test_null_like_kotlinc() {
    assert_identical(
        "PlatformStringWhenSwitch",
        PLATFORM_SOURCE,
        "store/PlatformStringWhenSwitchKt",
    );
}

#[test]
fn platform_string_switches_execute() {
    let out = common::compile_and_run_with_stdlib(
        &format!(
            "{PLATFORM_SOURCE}\nfun box(): String {{\n\
             \x20   val proxy = java.lang.reflect.Proxy.newProxyInstance(\n\
             \x20       Runnable::class.java.classLoader, arrayOf(Runnable::class.java), handler()\n\
             \x20   )\n\
             \x20   val any = Any::class.java\n\
             \x20   return listOf(\n\
             \x20       proxy.toString(), proxy.hashCode(), proxy.equals(proxy),\n\
             \x20       member(any.getMethod(\"hashCode\")), member(any.getMethod(\"getClass\")),\n\
             \x20       property(\"krusty.unset.property\"), declared(any.getMethod(\"toString\"))\n\
             \x20   ).toString()\n\
             }}\n"
        ),
        "PlatformStringWhenSwitchRun",
    );
    assert_eq!(out.as_deref(), Some("[proxy, 7, false, b, d, 3, 4]"));
}

#[test]
fn string_and_char_switches_execute() {
    let out = common::compile_and_run_with_stdlib(
        &format!(
            "{SOURCE}\nfun box(): String = listOf(\n\
             \x20   four(\"cde\"), four(\"ghi\"), four(\"x\"),\n\
             \x20   nullable(null), nullable(\"abc\"), nullable(\"zz\")\n\
             ).toString() + listOf(\n\
             \x20   nullableElse(null), nullableElse(\"c\"), collide(\"Aa\"), collide(\"BB\"), collide(\"D\"), collide(\"x\"),\n\
             \x20   two(\"b\"), many(\"thirteen\"), many(\"seven\"), many(\"x\"), chars('c'), chars('z'), chars('q')\n\
             )\n"
        ),
        "StringWhenSwitchRun",
    );
    assert_eq!(
        out.as_deref(),
        Some("[abc_cde, efg_ghi, other, efg_ghi, abc_cde, other][4, 3, 1, 2, 5, 6, 2, 13, 7, -1, 2, 3, 4]")
    );
}

#[test]
fn string_hash_bucket_switches_execute() {
    let out = common::compile_and_run_with_stdlib(
        &format!(
            "{HASH_BINS_SOURCE}\nfun box(): String = listOf(\n\
             \x20   treeifyResize(\"AD\"), treeifyResize(\"Bb\"), treeifyResize(\"Dd\"),\n\
             \x20   treeBin(\"Af\"), treeBin(\"Oo\"), treeBin(\"AI\"),\n\
             \x20   splitTreeBin(\"Aa\"), splitTreeBin(\"Iz\"), splitTreeBin(\"ix\"), splitTreeBin(\"zz\")\n\
             ).toString()\n"
        ),
        "StringWhenHashBinsRun",
    );
    assert_eq!(out.as_deref(), Some("[0, 5, 10, 0, 8, 15, 0, 24, 48, -1]"));
}
