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
        &[stdlib, reflect.clone()],
        Some(jdk.as_path()),
    )
    .expect("krusty box");
    let kotlinc = common::kotlinc_box_result_with_classpath(source, &[reflect]);
    assert_eq!(krusty, kotlinc);
    assert_eq!(krusty, "OK");
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
open class Common { fun common(): String = "common" }
interface Shared { fun shared(): String }
class Left : Common(), Shared { override fun shared(): String = "left" }
class Right : Common(), Shared { override fun shared(): String = "right" }

fun <T> sel(a: T, b: T) = a
inline fun <reified T> T.valueTypeOf() = typeOf<T>()

@Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER", "UNUSED_PARAMETER")
private fun <T> checkTypeEquality(
    first: @kotlin.internal.Exact T,
    second: @kotlin.internal.Exact T,
) {}

fun box(): String {
    val pair = sel(A, B)
    checkTypeEquality(pair, pair)
    if (pair.x() != "x" || pair.y() != "y") return "pair"
    val value = sel(Inv(A), Inv(B)).v
    if (value.x() != "x" || value.y() != "y") return "members"
    if (value.valueTypeOf() != typeOf<Any>()) return "any:${value.valueTypeOf()}"
    if (sel(Inv(C()), Inv(D())).v.valueTypeOf() != typeOf<Z>()) return "z:${sel(Inv(C()), Inv(D())).v.valueTypeOf()}"
    if (sel(Inv(L()), Inv(R())).v.valueTypeOf() != typeOf<Base>()) return "base"
    val classAndInterface = sel(Inv(Left()), Inv(Right())).v
    if (classAndInterface.common() != "common" || classAndInterface.shared() != "left") return "class-and-interface"
    return "OK"
}
"#,
    );
}

#[test]
fn a_flow_intersection_reifies_its_common_supertype() {
    // `K` is `A & B` from two smart casts. kotlinc reifies the whole intersection, so `K` is `Any`
    // for `typeOf`, for `is` and for the class literal, and not the first constituent `A`.
    agree(
        r#"
// LANGUAGE: -ProhibitIntersectionReifiedTypeParameter
import kotlin.reflect.typeOf

interface A
interface B
interface Z
interface P : Z
interface Q : Z
class C : A, B
class D : P, Q

var log = ""
inline fun <reified K> select(x: K, y: Any): K where K : A, K : B {
    log += "${y is K};${K::class == Any::class};${typeOf<K>() == typeOf<Any>()};"
    return x
}
inline fun <reified K> shared(x: K): Boolean where K : P, K : Q = typeOf<K>() == typeOf<Z>()

fun test(a: Any, b: Any) {
    if (a is A && a is B) select(a, b)
}

fun box(): String {
    test(C(), object : A {})
    test(C(), Any())
    if (log != "true;true;true;true;true;true;") return log
    val d: Any = D()
    if (d is P && d is Q && !shared(d)) return "z"
    return "OK"
}
"#,
    );
}
