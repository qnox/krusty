//! A compound index-assign `a[i] op= v` desugars to `a[i] = a[i] op v`, which must evaluate the
//! receiver and index EXACTLY ONCE (a side-effecting `a[f()] += v` calls `f()` once). krusty spills a
//! non-pure receiver/index to a temp before the read-modify-write. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn side_effecting_index_evaluated_once() {
    const SRC: &str = "val log = StringBuilder()\n\
fun ix(): Int { log.append(\"i\"); return 0 }\n\
fun box(): String {\n\
    val a = intArrayOf(10, 20)\n\
    a[ix()] += 5\n\
    val n = log.toString()\n\
    return if (a[0] == 15 && n == \"i\") \"OK\" else \"fail: a0=${a[0]} log='$n'\"\n\
}\n";
    assert_eq!(run(SRC).expect("compound index-assign side effect"), "OK");
}

#[test]
fn side_effecting_receiver_evaluated_once() {
    const SRC: &str = "val shared: IntArray = intArrayOf(1, 2)\n\
val log = StringBuilder()\n\
fun arr(): IntArray { log.append(\"a\"); return shared }\n\
fun box(): String {\n\
    arr()[0] += 10\n\
    val n = log.toString()\n\
    return if (shared[0] == 11 && n == \"a\") \"OK\" else \"fail: v=${shared[0]} log='$n'\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("compound index-assign receiver side effect"),
        "OK"
    );
}

#[test]
fn plain_compound_index_assign_still_works() {
    // A pure (local) index is left to re-evaluate — the value must still be correct.
    const SRC: &str = "fun box(): String {\n\
    val a = intArrayOf(1, 2, 3)\n\
    val i = 1\n\
    a[i] += 100\n\
    a[2] *= 4\n\
    return if (a[0] == 1 && a[1] == 102 && a[2] == 12) \"OK\" else \"fail: ${a[0]},${a[1]},${a[2]}\"\n\
}\n";
    assert_eq!(run(SRC).expect("plain compound index-assign"), "OK");
}
