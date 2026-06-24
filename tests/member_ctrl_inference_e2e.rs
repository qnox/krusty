//! A member (object/class) function with an *expression body* that is an `if`/`else` or `when`
//! expression infers its return type from the common type of the branches — like the equivalent
//! top-level function. The lightweight member-signature inferer previously handled only literals /
//! simple calls, so a control-flow body defaulted the return type to `Unit` and the method was
//! rejected with a spurious "expected 'Unit', actual '…'". Round-tripped under `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "C", &[sl], Some(&jdk))
}

#[test]
fn member_if_when_expr_body_infers_return() {
    const SRC: &str = "class Calc {\n\
    fun absLike(x: Int) = if (x > 0) x else -x\n\
    fun grade(score: Int) = when {\n\
        score >= 90 -> \"A\"\n\
        score >= 80 -> \"B\"\n\
        else -> \"C\"\n\
    }\n\
    fun widen(x: Int, y: Long) = if (x > 0) x.toLong() else y\n\
}\n\
fun box(): String {\n\
    val c = Calc()\n\
    if (c.absLike(-7) != 7) return \"fail abs: \" + c.absLike(-7)\n\
    if (c.absLike(4) != 4) return \"fail abs2\"\n\
    if (c.grade(95) != \"A\") return \"fail gradeA\"\n\
    if (c.grade(85) != \"B\") return \"fail gradeB\"\n\
    if (c.grade(10) != \"C\") return \"fail gradeC\"\n\
    if (c.widen(1, 9L) != 1L) return \"fail widen\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("member if/when expr body should compile + run");
    assert_eq!(out, "OK");
}
