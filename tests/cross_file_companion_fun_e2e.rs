//! A qualified `ClassName.fn(args)` call to a `companion object` FUNCTION where `ClassName` is declared
//! in ANOTHER FILE of the SAME MODULE. Same-file companion calls and classpath companion calls already
//! worked; the same-module-cross-file case recorded no lowering hint (the checker searched only the
//! current file's decls for the `Type$Companion` internal, and `class_internal` assumed the current
//! file's package) → the lowerer bailed "unrecorded qualified call target". The checker now falls back
//! to the module-wide `class_names` for the package-correct internal, and the lowerer emits the same
//! `getstatic Type.Companion; invokevirtual Type$Companion.fn(...)` shape the same-file path uses.
//! Compiled as ONE module (shared signatures) and round-tripped on the JVM.

use super::common;

use krusty::diag::DiagSink;
use krusty::frontend::{check_file, collect_signatures_with_cp};
use krusty::jvm::names::file_class_name;

/// Compile two sources as one module (mirrors `cross_file_ctor_default_e2e`'s harness).
fn compile_two(a: &str, b: &str) -> Option<Vec<(String, Vec<u8>)>> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;

    let mut diags = DiagSink::new();
    let features = krusty::features::LangFeatures::from_source(a);
    let parse = |s: &str, d: &mut DiagSink| {
        let toks = krusty::lexer::lex(s, d);
        krusty::parser::parse_with_features(s, &toks, d, &features)
    };
    let files = vec![parse(a, &mut diags), parse(b, &mut diags)];
    if diags.has_errors() {
        return None;
    }
    let cp_paths = vec![sl, jdk];
    let cp = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(cp_paths));
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone()));
    let mut syms = collect_signatures_with_cp(&files, platform, &mut diags);
    if diags.has_errors() {
        return None;
    }
    let stems = ["AKt", "BKt"];
    for (i, file) in files.iter().enumerate() {
        let facade = file_class_name(stems[i], file.package.as_deref());
        for &d in &file.decls {
            if let krusty::ast::Decl::Fun(f) = file.decl(d) {
                if f.receiver.is_none() && !f.is_inline {
                    let facade_name = krusty::types::type_name(&facade);
                    syms.fn_facades_by_decl.insert((i as u32, d.0), facade_name);
                    syms.fn_facades.insert(f.name.clone(), facade_name);
                }
            }
        }
    }
    let mut all = Vec::new();
    for (i, file) in files.iter().enumerate() {
        diags.set_file(i as u32);
        let info = check_file(file, &mut syms, &mut diags);
        if diags.has_errors() {
            return None;
        }
        let facade = file_class_name(stems[i], file.package.as_deref());
        let runtime = krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone());
        let mut ir = krusty::ir_lower::lower_file_at(file, i as u32, &info, &syms, &runtime)?;
        krusty::jvm::backend::run_backend_passes(&mut ir, file, &facade, "main", &syms).ok()?;
        all.extend(krusty::jvm::ir_emit::emit_all(&ir, &facade, &*cp, None)?);
    }
    Some(all)
}

fn run_two(a: &str, b: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let classes = compile_two(a, b)?;
    let box_class = common::find_box_class(&classes)?;
    common::run_box(&classes, &box_class, &[sl])
}

#[test]
fn cross_file_companion_function_call_runs() {
    if common::java_home().is_none() || common::stdlib_jar().is_none() {
        return;
    }
    // `Job` (file A) has a `companion object` with functions; file B calls them qualified across the
    // module boundary. Must emit `getstatic Job.Companion; invokevirtual Job$Companion.fn` and run.
    let a = "class Job(val id: String) {\n\
             companion object {\n\
             fun idle(): Job = Job(\"default\")\n\
             fun named(n: String): Job = Job(n)\n\
             }\n\
             }\n";
    let b = "fun box(): String {\n\
             if (Job.idle().id != \"default\") return \"f1\"\n\
             if (Job.named(\"x\").id != \"x\") return \"f2\"\n\
             return \"OK\"\n\
             }\n";
    assert_eq!(run_two(a, b).as_deref(), Some("OK"));
}
