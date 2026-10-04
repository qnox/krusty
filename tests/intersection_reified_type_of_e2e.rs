//! `sel(Inv(A), Inv(B))` captures the arguments' common supertype, and a reified `typeOf` of that
//! intersection uses the single classifier kotlinc reifies.
use std::path::PathBuf;

use super::common::{self, compile_and_run_box_files};

fn reflect_jar() -> PathBuf {
    common::dist_jar("kotlin-reflect.jar").unwrap_or_else(|| {
        panic!("kotlin-reflect.jar is required to run typeOf");
    })
}

fn agree(source: &str) {
    let reflect = reflect_jar();
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let krusty = compile_and_run_box_files(
        &[("main.kt", source)],
        &[stdlib.clone(), reflect.clone()],
        Some(jdk.as_path()),
    )
    .expect("krusty box");
    let kotlinc = kotlinc_box(source, &stdlib, &reflect);
    assert_eq!(krusty, kotlinc);
    assert_eq!(krusty, "OK");
}

fn kotlinc_box(source: &str, stdlib: &PathBuf, reflect: &PathBuf) -> String {
    let work = common::scratch_dir().expect("scratch");
    let source_path = work.join("main.kt");
    std::fs::write(&source_path, source).expect("write source");
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("output dir");
    let (code, diagnostics) = common::kotlinc_compile(&[
        source_path.display().to_string(),
        "-d".to_string(),
        output.display().to_string(),
        "-XXLanguage:-ProhibitIntersectionReifiedTypeParameter".to_string(),
    ])
    .expect("kotlinc");
    assert_eq!(code, 0, "kotlinc rejected the fixture: {diagnostics}");
    let result = common::run_box(&[], "MainKt", &[output, stdlib.clone(), reflect.clone()])
        .expect("kotlinc box");
    let _ = std::fs::remove_dir_all(work);
    result
}

#[test]
fn a_reified_intersection_uses_its_common_supertype() {
    agree(
        r#"
// LANGUAGE: -ProhibitIntersectionReifiedTypeParameter
import kotlin.reflect.typeOf

class Inv<T>(val v: T)
interface X { fun x(): String = "x" }
interface Y { fun y(): String = "y" }
interface Z
interface P : Z
interface Q : Z
object A : X, Y
object B : X, Y
class C : P, Q
class D : P, Q
open class Base
class L : Base()
class R : Base()

fun <T> sel(a: T, b: T) = a
inline fun <reified T> T.valueTypeOf() = typeOf<T>()

fun box(): String {
    val pair = sel(A, B)
    if (pair.x() != "x" || pair.y() != "y") return "pair"
    val value = sel(Inv(A), Inv(B)).v
    if (value.x() != "x" || value.y() != "y") return "members"
    if (value.valueTypeOf() != typeOf<Any>()) return "any:${value.valueTypeOf()}"
    if (sel(Inv(C()), Inv(D())).v.valueTypeOf() != typeOf<Z>()) return "z:${sel(Inv(C()), Inv(D())).v.valueTypeOf()}"
    if (sel(Inv(L()), Inv(R())).v.valueTypeOf() != typeOf<Base>()) return "base"
    return "OK"
}
"#,
    );
}
