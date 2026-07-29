//! Temporary: smart-cast gaps — when-is in local funs; elvis-return receiver narrowing.

use std::rc::Rc;

use krusty::features::LangFeatures;
use krusty::frontend;
use krusty::source::SourceInput;

fn check(label: &str, main_src: &str, classpath: Rc<krusty::jvm::classpath::Classpath>) {
    check_multi(label, &[main_src], 1, classpath);
}

fn check_multi(
    label: &str,
    srcs: &[&str],
    inferred: usize,
    classpath: Rc<krusty::jvm::classpath::Classpath>,
) {
    let inputs: Vec<SourceInput> = srcs.iter().map(|s| SourceInput::kotlin(s)).collect();
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(classpath));
    let mut diags = krusty::diag::DiagSink::new();
    let _ = frontend::analyze_source_set_prefix_with_features(
        &inputs,
        1,
        inferred,
        platform,
        &LangFeatures::new(),
        &mut diags,
    );
    let n = diags.diags.iter().filter(|d| d.file == 0).count();
    println!("== {label}: {n} diagnostics");
    for d in diags.diags.iter().filter(|d| d.file == 0) {
        println!("   {}..{} {}", d.span.lo, d.span.hi, d.msg);
    }
}

fn main() {
    let stdlib = krusty::toolchain::stdlib_jar().expect("stdlib");
    let jdk = krusty::toolchain::jdk_modules();
    let mut entries = vec![stdlib];
    entries.extend(jdk);
    let classpath = Rc::new(krusty::jvm::classpath::Classpath::new(entries));
    classpath.prepare_for_source_analysis();
    let java_sources = [
        (
            String::new(),
            "package p; public class UExpr { public UExpr getReceiver() { return null; } }"
                .to_string(),
        ),
        (
            String::new(),
            "package p; public class UQual extends UExpr { public String getName() { return \"\"; } }"
                .to_string(),
        ),
        (
            String::new(),
            "package p; public class UClazz { public String getJavaPsi() { return \"\"; } public int getSize() { return 0; } public UMeth[] getMethods() { return null; } }"
                .to_string(),
        ),
        (
            String::new(),
            "package p; public class UMeth { public String getJavaPsi() { return \"\"; } }"
                .to_string(),
        ),
        (
            String::new(),
            "package p; public class PsiM {}".to_string(),
        ),
        (
            String::new(),
            "package p; public interface PsiCls { PsiM[] getMethods(); }".to_string(),
        ),
    ];
    let stubs = krusty::jvm::java_stub::stub_classes(
        &java_sources,
        krusty::jvm::java_stub::StubMode::Lenient,
        &|candidate| {
            classpath
                .find_name(krusty::types::type_name(candidate))
                .is_some()
        },
    )
    .expect("stubs");
    classpath.set_stub_overlay(stubs);

    // A: when-is narrowing of a local-fun parameter's member, recursive call — the
    // qualifiedUtils asQualifiedPath shape.
    check(
        "A when-is in recursive local fun",
        "package a\nimport p.UExpr\nimport p.UQual\nfun path(root: UQual): Int {\n  var count = 0\n  fun add(expr: UQual) {\n    val receiver = expr.receiver\n    when (receiver) {\n      is UQual -> add(receiver)\n      else -> count += 1\n    }\n  }\n  add(root)\n  return count\n}\n",
        classpath.clone(),
    );
    // B: elvis-return narrows the safe-call receiver afterwards.
    check(
        "B elvis-return receiver narrowing",
        "package a\nimport p.UClazz\nfun f(u: UClazz?): Int {\n  val j = u?.javaPsi ?: return 0\n  println(j)\n  return u.size\n}\n",
        classpath.clone(),
    );

    // C: the real asQualifiedPath shape over a Kotlin interface hierarchy.
    // C1: support interfaces INSIDE the inferred prefix (fully checked).
    check_multi(
        "C1 asQualifiedPath, support checked",
        &[MAIN_C, SUPPORT_C],
        2,
        classpath.clone(),
    );
    // C2: support interfaces BEYOND the prefix (declaration-only tier).
    check_multi(
        "C2 asQualifiedPath, support decl-only",
        &[MAIN_C, SUPPORT_C],
        1,
        classpath.clone(),
    );
    // D: like C1 but the subject's initializer is a plain property read (no extension call).
    check_multi(
        "D no unwrapParenthesis",
        &[MAIN_D, SUPPORT_C],
        2,
        classpath.clone(),
    );
    // E: like C1 but without the selector elvis line.
    check_multi(
        "E no selector elvis",
        &[MAIN_E, SUPPORT_C],
        2,
        classpath.clone(),
    );
    // F: like C1 but no captured-var mutation in arms (returns instead).
    check_multi(
        "F no captures in arms",
        &[MAIN_F, SUPPORT_C],
        2,
        classpath.clone(),
    );
    check_multi(
        "G three arms, no captures",
        &[MAIN_G, SUPPORT_C],
        2,
        classpath.clone(),
    );
    check_multi(
        "H when statement, no captures",
        &[MAIN_H, SUPPORT_C],
        2,
        classpath.clone(),
    );
    check_multi(
        "I captured var in else arm",
        &[MAIN_I, SUPPORT_C],
        2,
        classpath.clone(),
    );
    check_multi(
        "J list capture in second arm",
        &[MAIN_J, SUPPORT_C],
        2,
        classpath.clone(),
    );
    check_multi(
        "K extension enclosing",
        &[MAIN_K, SUPPORT_C],
        2,
        classpath.clone(),
    );
    check_multi(
        "L is E minus head-if",
        &[MAIN_L, SUPPORT_C],
        2,
        classpath.clone(),
    );
    check_multi(
        "M is K plus head-if",
        &[MAIN_M, SUPPORT_C],
        2,
        classpath.clone(),
    );
    // N: `find(::pred)?.member` — a callable-ref predicate arg must still bind the
    // extension's element type parameter.
    check(
        "N find with callable-ref arg",
        "package a\nimport p.UClazz\nimport p.UMeth\nfun pred(m: UMeth): Boolean = true\nfun f(u: UClazz?): String? {\n  val jp = u?.javaPsi ?: return null\n  println(jp)\n  u.methods.find(::pred)?.javaPsi?.let { return it }\n  return null\n}\n",
        classpath.clone(),
    );
    // O: same shape but the element type is a KOTLIN source interface — checked and decl-only.
    check_multi(
        "O1 kotlin find target, checked",
        &[MAIN_O, SUPPORT_O],
        2,
        classpath.clone(),
    );
    check_multi(
        "O2 kotlin find target, decl-only",
        &[MAIN_O, SUPPORT_O],
        1,
        classpath.clone(),
    );
    // P: the Kotlin override SHADOWS a Java-supertype accessor pair (UClass.methods vs
    // PsiClass.getMethods): resolution must pick the source override's element type.
    check_multi(
        "P1 override vs java getter, checked",
        &[MAIN_P, SUPPORT_P],
        2,
        classpath.clone(),
    );
    check_multi(
        "P2 override vs java getter, decl-only",
        &[MAIN_P, SUPPORT_P],
        1,
        classpath.clone(),
    );
    // Q: the member is INHERITED through a source-interface chain (javaPsi lives two
    // supertypes up), element reached via find(::ref).
    check_multi(
        "Q1 inherited member, checked",
        &[MAIN_Q, SUPPORT_Q],
        2,
        classpath.clone(),
    );
    check_multi(
        "Q2 inherited member, decl-only",
        &[MAIN_Q, SUPPORT_Q],
        1,
        classpath.clone(),
    );
    // R: Kotlin override REFINES a Java-supertype getter's synthetic property
    // (`UClass.getMethods(): Array<UMethod>` over `PsiClass.getMethods(): PsiMethod[]`).
    check_multi(
        "R1 kotlin-refined getter, checked",
        &[MAIN_R, SUPPORT_R],
        2,
        classpath.clone(),
    );
    check_multi(
        "R2 kotlin-refined getter, decl-only",
        &[MAIN_R, SUPPORT_R],
        1,
        classpath.clone(),
    );
}

const SUPPORT_R: &str = r#"package u4

import p.PsiCls
import p.PsiM

interface UMethod2 : PsiM {
  val javaPsi: String
}

interface UClass2 : PsiCls {
  override fun getMethods(): Array<UMethod2>
}
"#;

const MAIN_R: &str = r#"package a

import u4.UClass2
import u4.UMethod2

fun pred(m: UMethod2): Boolean = true

fun f(u: UClass2?): String? {
  val x = u ?: return null
  x.methods.find(::pred)?.javaPsi?.let { return it }
  return null
}
"#;

const SUPPORT_Q: &str = r#"package u3

interface UElem {
  val javaPsi: String
}

interface UDecl : UElem

interface UMethod2 : UDecl

interface UClass2 {
  val methods: Array<UMethod2>
}
"#;

const MAIN_Q: &str = r#"package a

import u3.UClass2
import u3.UMethod2

fun pred(m: UMethod2): Boolean = true

fun f(u: UClass2?): String? {
  val ms = u?.methods ?: return null
  ms.find(::pred)?.javaPsi?.let { return it }
  return null
}
"#;

const SUPPORT_P: &str = r#"package u2

import p.PsiCls

interface UMethod {
  val javaPsi: String
}

interface UClass : PsiCls {
  val methods: Array<UMethod>
}
"#;

const MAIN_P: &str = r#"package a

import u2.UClass
import u2.UMethod

fun pred(m: UMethod): Boolean = true

fun f(u: UClass?): String? {
  u?.methods?.find(::pred)?.javaPsi?.let { return it }
  return null
}
"#;

const SUPPORT_O: &str = r#"package u2

interface UMethod {
  val javaPsi: String
}

interface UClass {
  val javaPsi: String
  val methods: Array<UMethod>
}
"#;

const MAIN_O: &str = r#"package a

import u2.UClass
import u2.UMethod

fun pred(m: UMethod): Boolean = true

fun f(u: UClass?): String? {
  val jp = u?.javaPsi ?: return null
  println(jp)
  u.methods.find(::pred)?.javaPsi?.let { return it }
  return null
}
"#;

const MAIN_L: &str = r#"package u

fun UExpression.asQualifiedPath(): List<String>? {
  if (this !is UQualified) {
    return null
  }

  var error = false
  val list = mutableListOf<String>()
  fun addIdentifiers(expr: UQualified) {
    val receiver = expr.receiver.unwrapParenthesis()
    when (receiver) {
      is UQualified -> addIdentifiers(receiver)
      is USimpleName -> list += receiver.identifier
      else -> {
        error = true
        return
      }
    }
  }

  addIdentifiers(this)
  return if (error) null else list
}
"#;

const MAIN_M: &str = r#"package u

fun UExpression.asQualifiedPath2(): Int {
  if (this is USimpleName) {
    return this.identifier.length
  }
  else if (this !is UQualified) {
    return 0
  }
  fun addIdentifiers(expr: UQualified): Int {
    val receiver = expr.receiver.unwrapParenthesis()
    return when (receiver) {
      is UQualified -> addIdentifiers(receiver)
      else -> 0
    }
  }
  return addIdentifiers(this)
}
"#;

const MAIN_J: &str = r#"package u

fun f(root: UQualified): Int {
  val list = mutableListOf<String>()
  fun addIdentifiers(expr: UQualified): Int {
    val receiver = expr.receiver.unwrapParenthesis()
    return when (receiver) {
      is UQualified -> addIdentifiers(receiver)
      is USimpleName -> { list += receiver.identifier; 1 }
      else -> 0
    }
  }
  return addIdentifiers(root)
}
"#;

const MAIN_K: &str = r#"package u

fun UExpression.asQualifiedPath2(): Int {
  if (this !is UQualified) {
    return 0
  }
  fun addIdentifiers(expr: UQualified): Int {
    val receiver = expr.receiver.unwrapParenthesis()
    return when (receiver) {
      is UQualified -> addIdentifiers(receiver)
      else -> 0
    }
  }
  return addIdentifiers(this)
}
"#;

const MAIN_G: &str = r#"package u

fun f(root: UQualified): Int {
  fun addIdentifiers(expr: UQualified): Int {
    val receiver = expr.receiver.unwrapParenthesis()
    return when (receiver) {
      is UQualified -> addIdentifiers(receiver)
      is USimpleName -> receiver.identifier.length
      else -> 0
    }
  }
  return addIdentifiers(root)
}
"#;

const MAIN_H: &str = r#"package u

fun f(root: UQualified) {
  fun addIdentifiers(expr: UQualified) {
    val receiver = expr.receiver.unwrapParenthesis()
    when (receiver) {
      is UQualified -> addIdentifiers(receiver)
      else -> {}
    }
  }
  addIdentifiers(root)
}
"#;

const MAIN_I: &str = r#"package u

fun f(root: UQualified): Boolean {
  var error = false
  fun addIdentifiers(expr: UQualified) {
    val receiver = expr.receiver.unwrapParenthesis()
    when (receiver) {
      is UQualified -> addIdentifiers(receiver)
      else -> {
        error = true
        return
      }
    }
  }
  addIdentifiers(root)
  return error
}
"#;

const MAIN_D: &str = r#"package u

fun UExpression.asQualifiedPath(): List<String>? {
  if (this is USimpleName) {
    return listOf(this.identifier)
  }
  else if (this !is UQualified) {
    return null
  }

  var error = false
  val list = mutableListOf<String>()
  fun addIdentifiers(expr: UQualified) {
    val receiver = expr.receiver
    val selector = expr.selector as? USimpleName ?: run { error = true; return }
    when (receiver) {
      is UQualified -> addIdentifiers(receiver)
      is USimpleName -> list += receiver.identifier
      else -> {
        error = true
        return
      }
    }
    list += selector.identifier
  }

  addIdentifiers(this)
  return if (error) null else list
}
"#;

const MAIN_E: &str = r#"package u

fun UExpression.asQualifiedPath(): List<String>? {
  if (this is USimpleName) {
    return listOf(this.identifier)
  }
  else if (this !is UQualified) {
    return null
  }

  var error = false
  val list = mutableListOf<String>()
  fun addIdentifiers(expr: UQualified) {
    val receiver = expr.receiver.unwrapParenthesis()
    when (receiver) {
      is UQualified -> addIdentifiers(receiver)
      is USimpleName -> list += receiver.identifier
      else -> {
        error = true
        return
      }
    }
  }

  addIdentifiers(this)
  return if (error) null else list
}
"#;

const MAIN_F: &str = r#"package u

fun UExpression.asQualifiedPath(): List<String>? {
  if (this !is UQualified) {
    return null
  }

  fun addIdentifiers(expr: UQualified): Int {
    val receiver = expr.receiver.unwrapParenthesis()
    return when (receiver) {
      is UQualified -> addIdentifiers(receiver)
      else -> 0
    }
  }

  addIdentifiers(this)
  return null
}
"#;

const SUPPORT_C: &str = r#"package u

interface UElement

interface UExpression : UElement

interface UReference : UExpression

interface UQualified : UReference {
  val receiver: UExpression
  val selector: UExpression
}

interface USimpleName : UReference {
  val identifier: String
}

interface UParen : UExpression {
  val expression: UExpression
}

fun UExpression.unwrapParenthesis(): UExpression = (this as? UParen)?.expression ?: this
"#;

const MAIN_C: &str = r#"package u

fun UExpression.asQualifiedPath(): List<String>? {
  if (this is USimpleName) {
    return listOf(this.identifier)
  }
  else if (this !is UQualified) {
    return null
  }

  var error = false
  val list = mutableListOf<String>()
  fun addIdentifiers(expr: UQualified) {
    val receiver = expr.receiver.unwrapParenthesis()
    val selector = expr.selector as? USimpleName ?: run { error = true; return }
    when (receiver) {
      is UQualified -> addIdentifiers(receiver)
      is USimpleName -> list += receiver.identifier
      else -> {
        error = true
        return
      }
    }
    list += selector.identifier
  }

  addIdentifiers(this)
  return if (error) null else list
}
"#;
