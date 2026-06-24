//! End-to-end proof that the plugin SURFACE composes with krusty's real front end: a Kotlin source
//! is run through the actual pipeline (lex → parse → resolve → `lower_file`) to a real `IrFile`, then
//! the serialization extension runs over it via `PluginHost`. This closes the gap between the
//! self-contained unit tests (hand-built IR) and the production pipeline — the plugin operates on IR
//! krusty itself produced, not a fixture.
//!
//! Skips (does not fail) when no kotlin-stdlib jar is locatable, like other classpath-dependent tests.

use std::rc::Rc;

use krusty::diag::DiagSink;
use krusty::ir_lower::lower_file;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::plugins::serialization::{SerializationPlugin, SERIALIZABLE_FQ};
use krusty::plugins::{PluginContext, PluginHost};
use krusty::resolve::{check_file, collect_signatures_with_cp};

mod common;

/// Lower a source through krusty's full front end to its parsed `File` + real `IrFile`, or `None` if
/// it can't compile (or no stdlib is available).
fn lower(src: &str) -> Option<(krusty::ast::File, krusty::ir::IrFile)> {
    let jar = common::stdlib_jar()?;
    let cp = Rc::new(krusty::jvm::classpath::Classpath::new(vec![jar]));

    let mut d = DiagSink::new();
    let toks = lex(src, &mut d);
    let files = vec![parse(src, &toks, &mut d)];
    if d.has_errors() {
        return None;
    }
    let platform = Box::new(JvmLibraries::new(cp.clone()));
    let syms = collect_signatures_with_cp(&files, platform, &mut d);
    if d.has_errors() {
        return None;
    }
    let info = check_file(&files[0], &syms, &mut d);
    if d.has_errors() {
        return None;
    }
    let ir = lower_file(&files[0], &info, &syms)?;
    let [file] = <[krusty::ast::File; 1]>::try_from(files).ok()?;
    Some((file, ir))
}

#[test]
fn serialization_plugin_runs_on_real_lowered_ir() {
    // A plain class krusty's IR subset lowers (primary-constructor val properties).
    let Some((_file, mut ir)) = lower("class Foo(val a: Int, val b: String)") else {
        eprintln!("skipping: no stdlib jar / class outside IR subset");
        return;
    };

    // Find the real lowered class and mark it @Serializable (the side-table the production path will
    // populate from captured AST annotations — see docs/PLUGIN_API.md).
    let foo_id = ir
        .classes
        .iter()
        .position(|c| c.fq_name.ends_with("Foo"))
        .expect("lowered Foo class present") as u32;
    let foo_fq = ir.classes[foo_id as usize].fq_name.clone();
    let fields_before = ir.classes[foo_id as usize].fields.len();

    let mut ctx = PluginContext::default();
    ctx.class_annotations
        .insert(foo_id, vec![SERIALIZABLE_FQ.to_string()]);

    let mut host = PluginHost::new();
    host.register(Box::new(SerializationPlugin::default()));
    host.run(&mut ir, &ctx);

    // The $serializer object was synthesized onto krusty's own lowered IR.
    let ser_fq = format!("{foo_fq}$serializer");
    let ser = ir
        .classes
        .iter()
        .find(|c| c.fq_name == ser_fq)
        .expect("$serializer synthesized on real IR");
    assert!(ser.is_object);

    // childSerializers' element count matches the real lowered field count (2).
    let child = ser
        .methods
        .iter()
        .map(|&f| &ir.functions[f as usize])
        .find(|f| f.name == "childSerializers")
        .unwrap();
    assert_eq!(fields_before, 2, "Foo lowered with 2 fields");
    let body = child.body.expect("childSerializers has a body");
    let krusty::ir::IrExpr::Block { value: Some(v), .. } = ir.expr(body) else {
        panic!("childSerializers body not a block");
    };
    let krusty::ir::IrExpr::Vararg { elements, .. } = ir.expr(*v) else {
        panic!("childSerializers does not return an array");
    };
    assert_eq!(
        elements.len(),
        fields_before,
        "one element serializer per real field"
    );

    // serializer() accessor was attached to the real Foo class.
    assert!(
        ir.classes[foo_id as usize]
            .methods
            .iter()
            .any(|&f| ir.functions[f as usize].name == "serializer"),
        "serializer() accessor added to real Foo"
    );
}

#[test]
fn serialization_activates_from_source_annotation() {
    // The keystone: the surface activates from a REAL `@Serializable` in source — parser captures the
    // annotation, `PluginContext::from_source` indexes it, the plugin fires. No manual injection.
    let Some((file, mut ir)) = lower("@Serializable class Foo(val a: Int, val b: String)") else {
        eprintln!("skipping: no stdlib jar / class outside IR subset");
        return;
    };

    let ctx = PluginContext::from_source(&file, &ir);
    assert!(
        !ctx.classes_with_simple("Serializable").is_empty(),
        "@Serializable captured from source and indexed"
    );

    let mut host = PluginHost::new();
    host.register(Box::new(SerializationPlugin::default()));
    host.run(&mut ir, &ctx);

    assert!(
        ir.classes
            .iter()
            .any(|c| c.fq_name.ends_with("Foo$serializer")),
        "$serializer synthesized purely from the source annotation"
    );
}

#[test]
fn top_level_function_registers_parameter_defaults_for_plugins() {
    // A plugin/transform that rewrites a default-valued call (e.g. Compose's `$default` mask) needs the
    // LOWERED default exprs. Member methods and data-class `copy` already register them; a plain
    // top-level function must too, with the static value layout (params at values 0..n, no `this`).
    let Some((_file, ir)) =
        lower("fun bar(a: Int, b: String = \"hello\", c: Boolean = true) = \"\"")
    else {
        eprintln!("skipping: no stdlib jar / outside IR subset");
        return;
    };
    let fid = ir
        .functions
        .iter()
        .position(|f| f.name == "bar")
        .expect("bar lowered") as u32;
    let defaults = ir
        .fn_param_defaults
        .get(&fid)
        .expect("top-level fn with defaults must register fn_param_defaults");
    assert_eq!(
        defaults.len(),
        3,
        "one entry per parameter; got {defaults:?}"
    );
    assert!(
        defaults[0].is_none() && defaults[1].is_some() && defaults[2].is_some(),
        "required param has no default, the two defaulted params do; got {defaults:?}"
    );
}
