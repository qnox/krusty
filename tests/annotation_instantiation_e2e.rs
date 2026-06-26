//! Kotlin annotation INSTANTIATION (Kotlin 1.6+): calling an `annotation class`'s constructor (`A(5)`)
//! constructs an annotation instance whose members are readable (`a.x`) and whose `equals`/`hashCode`/
//! `toString` follow the `java.lang.annotation.Annotation` contract (content equality, `Arrays`-aware,
//! `Float`/`Double` NaN/`-0.0` semantics). krusty emits the annotation as a JVM annotation interface plus
//! a synthetic impl class implementing that contract. Verified by running `box()` on a real JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let stdlib = common::stdlib_jar()?;
    let jdk = std::env::var("JAVA_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(|jh| std::path::PathBuf::from(format!("{jh}/lib/modules")));
    let cp = std::slice::from_ref(&stdlib);
    let classes = common::compile_in_process(src, "T", cp, jdk.as_deref())?;
    common::run_box(&classes, "TKt", cp)
}

#[test]
fn annotation_instantiation_member_read_and_content_equality() {
    let src = "annotation class A(val x: Int, val s: String)\n\
        fun box(): String {\n\
        \x20 val a = A(5, \"hi\"); val b = A(5, \"hi\"); val c = A(6, \"hi\")\n\
        \x20 if (a.x != 5 || a.s != \"hi\") return \"read\"\n\
        \x20 if (a != b) return \"eq\"\n\
        \x20 if (a.hashCode() != b.hashCode()) return \"hash\"\n\
        \x20 if (a == c) return \"neq\"\n\
        \x20 return \"OK\"\n}\n";
    match run(src) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: no toolchain"),
    }
}

#[test]
fn annotation_instantiation_array_member_content_equality() {
    let src = "annotation class A(val xs: IntArray)\n\
        fun box(): String {\n\
        \x20 val a: Any = A(intArrayOf(1, 2, 3)); val b: Any = A(intArrayOf(1, 2, 3)); val c: Any = A(intArrayOf(1, 3, 2))\n\
        \x20 if (!a.equals(b)) return \"eq\"\n\
        \x20 if (a.hashCode() != b.hashCode()) return \"hash\"\n\
        \x20 if (a.equals(c)) return \"neq\"\n\
        \x20 return \"OK\"\n}\n";
    match run(src) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: no toolchain"),
    }
}

#[test]
fn annotation_instantiation_nested_and_tostring() {
    let src = "annotation class Inner(val v: String)\n\
        annotation class Outer(val inn: Inner, val n: Int)\n\
        fun box(): String {\n\
        \x20 val a = Outer(Inner(\"q\"), 7); val b = Outer(Inner(\"q\"), 7); val c = Outer(Inner(\"z\"), 7)\n\
        \x20 if (a != b) return \"eq\"\n\
        \x20 if (a == c) return \"neq\"\n\
        \x20 if (a.inn.v != \"q\") return \"read\"\n\
        \x20 if (!a.toString().contains(\"Outer(\")) return \"toString:\" + a.toString()\n\
        \x20 return \"OK\"\n}\n";
    match run(src) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: no toolchain"),
    }
}
