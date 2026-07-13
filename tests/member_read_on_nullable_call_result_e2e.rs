//! A direct member read on the RESULT of a cross-file call whose return type is nullable
//! (`repo.findById(id)!!.builtIn`, or a `require(x != null)`-smart-cast local) — the shape mission-core's
//! RbacService `deleteRole` uses. krusty does not propagate a `!!` / smart-cast narrowing to a
//! call-result LOCAL's read site, so the receiver type stayed `Foo?`; `lower_member_read_on` then matched
//! only `Ty::Obj` (non-null) and resolved no member, so the file bailed. It now resolves against the
//! non-null receiver type (the value is a valid reference at runtime; krusty does not enforce null-safety).
use super::common;

const LIB: &str = "package lib\n\
    class Box(val flag: Boolean, val name: String)\n\
    interface Repo { fun find(b: Boolean): Box? }\n\
    object R : Repo { override fun find(b: Boolean): Box? = if (b) Box(b, \"hi\") else null }\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, jdk.clone()], Some(&jdk))
}

#[test]
fn member_read_on_not_null_asserted_call_result() {
    const MAIN: &str = "import lib.*\n\
        fun pick(r: Repo, b: Boolean): String = r.find(b)!!.name\n\
        fun box(): String = if (pick(R, true) == \"hi\") \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run("nn_call", MAIN).expect("member read on !! call result"),
        "OK"
    );
}

#[test]
fn member_read_on_require_smartcast_call_result() {
    const MAIN: &str = "import lib.*\n\
        fun flagOf(r: Repo, b: Boolean): Boolean {\n\
            val box = r.find(b)\n\
            require(box != null) { \"missing\" }\n\
            return box.flag\n\
        }\n\
        fun box(): String = if (flagOf(R, true)) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run("rq_call", MAIN).expect("member read on require-smartcast call result"),
        "OK"
    );
}
