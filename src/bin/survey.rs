use krusty::diag::DiagSink;
use krusty::ir_lower::lower_file;
use krusty::jvm::classpath::Classpath;
use krusty::jvm::ir_emit::emit_all;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::jvm::names::file_class_name;
use krusty::jvm::value_classes::lower_value_classes;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures_with_cp};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

/// Run the FULL pipeline (lex→parse→sigs→check→lower→value-classes→emit) against the real
/// classpath (stdlib + JDK `lib/modules`), so skip reasons match the conformance harness — not a
/// stdlib-less front-end-only approximation. Returns the first error (with a stage prefix for the
/// silent lower/emit bailouts that carry no diagnostic).
fn first_error(src: &str, cp: &Rc<Classpath>, stem: &str) -> Option<String> {
    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let files = vec![parse(src, &toks, &mut d)];
    if d.has_errors() {
        return Some(d.diags[0].msg.clone());
    }
    let platform = Box::new(JvmLibraries::new(cp.clone()));
    let syms = collect_signatures_with_cp(&files, platform, &mut d);
    if d.has_errors() {
        return Some(d.diags[0].msg.clone());
    }
    let info = check_file(&files[0], &syms, &mut d);
    if d.has_errors() {
        return Some(d.diags[0].msg.clone());
    }
    let facade = file_class_name(stem, files[0].package.as_deref());
    let mut ir = match lower_file(&files[0], &info, &syms) {
        Some(ir) => ir,
        None => return Some("lower: lower_file bailed (unsupported IR construct)".into()),
    };
    if !lower_value_classes(&mut ir) {
        return Some("lower: value-class shape not lowered".into());
    }
    if !krusty::jvm::suspend::lower_suspend(&mut ir, &facade) {
        return Some("lower: suspend-function shape not lowered".into());
    }
    match emit_all(&ir, &facade, &**cp, None) {
        Some(o) if !o.is_empty() => None,
        _ => Some("emit: emit_all bailed (unsupported codegen)".into()),
    }
}

fn categorize(err: &str) -> String {
    if err.contains("class bodies support") {
        return "nested decl in class body".into();
    }
    if err.contains("interface default") {
        return "interface default method".into();
    }
    if err.contains("mutable local variable") {
        return "mutable lambda capture".into();
    }
    if err.contains("bridge") {
        return "bridge method".into();
    }
    if err.contains("nullable primitive") || err.ends_with("? is not supported") {
        return "nullable primitive".into();
    }
    if err.contains("value/inline") || err.contains("inline class") {
        return "value/inline class".into();
    }
    if err.contains("secondary constructor") {
        return "secondary constructor".into();
    }
    if err.contains("conflicting declarations") {
        return "conflicting declarations".into();
    }
    if err.starts_with("lower:") || err.starts_with("emit:") {
        return err[..err.len().min(70)].to_string();
    }
    if err.contains("krusty: ") {
        let m = err.trim_start_matches("krusty: ");
        return format!("krusty: {}", &m[..m.len().min(60)]);
    }
    if err.contains("expected") {
        return format!("parse: {}", &err[..err.len().min(60)]);
    }
    format!("other: {}", &err[..err.len().min(60)])
}

fn collect_kt(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut es: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        es.sort();
        for p in es {
            if p.is_dir() {
                collect_kt(&p, out);
            } else if p.extension().is_some_and(|e| e == "kt") {
                out.push(p);
            }
        }
    }
}

/// Build the survey's `-classpath`: the kotlin-stdlib family jars + the JDK `lib/modules` jimage,
/// located the same way the conformance gate locates them. `KRUSTY_SURVEY_STDLIB` (`:`-separated)
/// and `KRUSTY_SURVEY_JDK_MODULES` override the located stdlib jars / JDK image respectively.
fn locate_classpath() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    match std::env::var("KRUSTY_SURVEY_STDLIB") {
        Ok(p) if !p.is_empty() => {
            for part in p.split(':').filter(|s| !s.is_empty()) {
                paths.push(PathBuf::from(part));
            }
        }
        // Auto-locate the same family the gate compiles against. Each prefix is optional: a missing
        // jar just means tests needing it stay (correctly) blocked, not falsely so for the others.
        _ => {
            for (prefix, excludes) in [
                ("kotlin-stdlib-", &["jdk7", "jdk8"][..]),
                ("kotlin-stdlib-jdk8", &[][..]),
                ("kotlin-test-", &["junit", "testng", "annotations"][..]),
                ("kotlin-reflect-", &[][..]),
                ("kotlinx-coroutines-core", &["jdk8"][..]),
                ("annotations-", &[][..]),
            ] {
                if let Some(j) = find_jar(prefix, excludes) {
                    paths.push(j);
                }
            }
        }
    }
    // JDK bootclasspath jimage: explicit override, else derive from the running/reference JDK home.
    let jdk_modules = std::env::var("KRUSTY_SURVEY_JDK_MODULES")
        .ok()
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let home = std::env::var("JAVA_HOME")
                .or_else(|_| std::env::var("KRUSTY_REF_JAVA_HOME"))
                .ok()?;
            let p = PathBuf::from(home).join("lib").join("modules");
            p.is_file().then_some(p)
        });
    if let Some(p) = jdk_modules {
        paths.push(p);
    }
    paths
}

/// Newest jar whose file name starts with `prefix` (and isn't a sources/js/wasm/excluded variant),
/// searched across the same locations the conformance gate uses: its Maven-download cache
/// (`~/.cache/krusty-deps`, where `common::ensure_maven` puts kotlin-test/reflect/coroutines/
/// annotations), the reference-compiler dist `lib/`, and the local `~/.gradle` / `~/.m2` caches.
/// `KRUSTY_DEPS_CACHE` overrides the cache dir.
fn find_jar(prefix: &str, excludes: &[&str]) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let deps_cache =
        std::env::var("KRUSTY_DEPS_CACHE").unwrap_or_else(|_| format!("{home}/.cache/krusty-deps"));
    let mut roots = vec![
        deps_cache,
        format!("{home}/.gradle"),
        format!("{home}/.m2/repository"),
    ];
    // The reference-compiler dist ships the exact jars; `KRUSTY_KOTLINC` points at its `bin/kotlinc`.
    if let Ok(kc) = std::env::var("KRUSTY_KOTLINC") {
        if let Some(lib) = std::path::Path::new(&kc)
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("lib"))
        {
            roots.insert(0, lib.to_string_lossy().into_owned());
        }
    }
    let mut found = Vec::new();
    for r in &roots {
        collect_named_jars(std::path::Path::new(r), prefix, excludes, &mut found, 0);
    }
    // Prefer the shortest name (the plain `<prefix><version>.jar`, not `-junit`/`-jvm`/…).
    found.sort_by_key(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.len())
            .unwrap_or(usize::MAX)
    });
    found.into_iter().next()
}

fn collect_named_jars(
    dir: &std::path::Path,
    prefix: &str,
    excludes: &[&str],
    out: &mut Vec<PathBuf>,
    depth: usize,
) {
    if depth > 9 || out.len() > 8 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_named_jars(&p, prefix, excludes, out, depth + 1);
        } else if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            let bad = ["sources", "javadoc", "-js", "wasm", "common", "metadata"];
            if name.starts_with(prefix)
                && name.ends_with(".jar")
                && !bad.iter().any(|b| name.contains(b))
                && !excludes.iter().any(|b| name.contains(b))
            {
                out.push(p);
            }
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let box_dir = args
        .next()
        .expect("usage: survey <box_dir> [--samples <category>]");
    let samples_cat = if args.next().as_deref() == Some("--samples") {
        args.next()
    } else {
        None
    };

    // Real classpath: the full kotlin-stdlib family + JDK `lib/modules`, so skip reasons match the
    // conformance gate (which puts stdlib + kotlin-test + reflect + coroutines + annotations on the
    // compile classpath). The survey LOCATES these itself — relying on a hand-set env var meant a
    // partial classpath turned resolvable references (`kotlin.test.*`, coroutines) into false blockers.
    // Env vars stay as explicit overrides for a pinned/reproducible run.
    let cp_paths = locate_classpath();
    let cp = Rc::new(Classpath::new(cp_paths));

    let mut errors: HashMap<String, Vec<String>> = HashMap::new();
    let mut scanned = 0u32;
    let mut compiled = 0u32;
    let mut files = Vec::new();
    collect_kt(std::path::Path::new(&box_dir), &mut files);
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap_or_default();
        let src = src.replace("OPTIONAL_JVM_INLINE_ANNOTATION", "@JvmInline");
        if src.contains("// FILE:") || src.contains("// MODULE:") {
            continue;
        }
        if !src.contains("fun box()") {
            continue;
        }
        if src.contains("// LAMBDAS: INDY") || src.contains("IGNORE_BACKEND_K2: JVM_IR") {
            continue;
        }
        if let Some(tb) = src.lines().find(|l| l.starts_with("// TARGET_BACKEND:")) {
            let t = tb.trim_start_matches("// TARGET_BACKEND:").trim();
            if !t.split(',').any(|x| matches!(x.trim(), "JVM" | "JVM_IR")) {
                continue;
            }
        }
        scanned += 1;
        let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or("File");
        match first_error(&src, &cp, stem) {
            None => compiled += 1,
            Some(e) => {
                let cat = categorize(&e);
                errors
                    .entry(cat)
                    .or_default()
                    .push(f.to_string_lossy().to_string());
            }
        }
    }
    let total_skip: u32 = errors.values().map(|v| v.len() as u32).sum();
    println!("Scanned: {scanned}  Compiled: {compiled}  Skip-errors: {total_skip}");
    let mut sorted: Vec<_> = errors.iter().collect();
    sorted.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    if let Some(cat) = &samples_cat {
        for (k, files) in &sorted {
            if k.contains(cat.as_str()) {
                println!("Category: {k} ({} files)", files.len());
                for f in files.iter() {
                    println!("{f}");
                }
            }
        }
    } else {
        for (k, v) in &sorted {
            println!("  {:4}  {k}", v.len());
        }
    }
}
