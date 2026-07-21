//! End-to-end proof that the plugin SURFACE composes with krusty's real front end: a Kotlin source
//! is run through the actual pipeline (lex → parse → resolve → `lower_file`) to a real `IrFile`, then
//! the serialization extension runs over it via `PluginHost`. This closes the gap between the
//! self-contained unit tests (hand-built IR) and the production pipeline — the plugin operates on IR
//! krusty itself produced, not a fixture.
//!
//! Skips (does not fail) when no kotlin-stdlib jar is locatable, like other classpath-dependent tests.

use std::rc::Rc;

use krusty::diag::DiagSink;
use krusty::frontend::{check_file, collect_signatures_with_cp};
use krusty::ir_lower::lower_file;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::plugins::serialization::{SerializationPlugin, SERIALIZABLE_FQ};
use krusty::plugins::{PluginContext, PluginHost};

use super::common;

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
    let mut syms = collect_signatures_with_cp(&files, platform, &mut d);
    if d.has_errors() {
        return None;
    }
    let info = check_file(&files[0], &mut syms, &mut d);
    if d.has_errors() {
        return None;
    }
    let runtime = JvmLibraries::new(cp.clone());
    let ir = lower_file(&files[0], &info, &syms, &runtime)?;
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
        .position(|c| c.fq_name().ends_with("Foo"))
        .expect("lowered Foo class present") as u32;
    let foo_fq = ir.classes[foo_id as usize].fq_name();
    let fields_before = ir.classes[foo_id as usize].fields.len();

    let mut ctx = PluginContext::default();
    ctx.class_annotations
        .insert(foo_id, vec![SERIALIZABLE_FQ.to_string()].into());

    let mut host = PluginHost::new();
    host.register(Box::new(SerializationPlugin::default()));
    host.run(&mut ir, &ctx);

    // The $serializer object was synthesized onto krusty's own lowered IR.
    let ser_fq = format!("{foo_fq}$$serializer");
    let ser = ir
        .classes
        .iter()
        .find(|c| c.fq_name_matches(&ser_fq))
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
    // The body is `{ return <array> }` — a block whose single statement returns the element-serializer
    // array (a non-Unit method needs an explicit `Return`; a block tail-value would never `areturn`).
    let krusty::ir::IrExpr::Block { stmts, .. } = ir.expr(body) else {
        panic!("childSerializers body not a block");
    };
    let krusty::ir::IrExpr::Return(Some(v)) = ir.expr(stmts[stmts.len() - 1]) else {
        panic!("childSerializers does not return");
    };
    let krusty::ir::IrExpr::Vararg { elements, .. } = ir.expr(*v) else {
        panic!("childSerializers does not return an array");
    };
    assert_eq!(
        elements.len(),
        fields_before,
        "one element serializer per real field"
    );

    // serializer() accessor was relocated to Foo's synthesized Companion (kotlinc's placement); Foo
    // itself carries a `companion_class` pointing at it.
    let comp_fq = ir.classes[foo_id as usize]
        .companion_class
        .expect("Foo has a companion_class for the serializer()");
    let comp = ir
        .classes
        .iter()
        .find(|c| c.fq_name == comp_fq)
        .expect("Foo$Companion synthesized");
    assert!(
        comp.is_companion
            && comp
                .methods
                .iter()
                .any(|&f| ir.functions[f as usize].name == "serializer"),
        "serializer() accessor added to Foo's Companion"
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
            .any(|c| c.fq_name().ends_with("Foo$$serializer")),
        "$serializer synthesized purely from the source annotation"
    );
}

#[test]
fn serializable_enum_relocates_serializer_to_companion() {
    // A `@Serializable enum` (kotlinx ≥ 1.5) gets `serializer()` on a nested `Companion` backed by a
    // cached `Lazy` delegate — NOT a static `serializer()` on the enum itself. Matches kotlinc's member
    // set (Companion class + `access$get$cachedSerializer$delegate$cp` accessor).
    let Some((_file, mut ir)) = lower("enum class E { A, B }") else {
        eprintln!("skipping: no stdlib jar / class outside IR subset");
        return;
    };

    let e_id = ir
        .classes
        .iter()
        .position(|c| c.fq_name().ends_with("E"))
        .expect("lowered E enum present") as u32;

    let mut ctx = PluginContext::default();
    ctx.class_annotations
        .insert(e_id, vec![SERIALIZABLE_FQ.to_string()].into());

    let mut host = PluginHost::new();
    host.register(Box::new(SerializationPlugin::default()));
    host.run(&mut ir, &ctx);

    // serializer() lives on E$Companion, not statically on E.
    let comp_fq = ir.classes[e_id as usize]
        .companion_class
        .expect("enum gets a companion_class for serializer()");
    let comp = ir
        .classes
        .iter()
        .find(|c| c.fq_name == comp_fq)
        .expect("E$Companion synthesized");
    assert!(
        comp.methods
            .iter()
            .any(|&f| ir.functions[f as usize].name == "serializer"),
        "serializer() on the Companion"
    );
    let enum_methods: Vec<&str> = ir.classes[e_id as usize]
        .methods
        .iter()
        .map(|&f| ir.functions[f as usize].name.as_str())
        .collect();
    assert!(
        !enum_methods.contains(&"serializer"),
        "enum keeps NO static serializer()"
    );
    assert!(
        enum_methods.contains(&"access$get$cachedSerializer$delegate$cp"),
        "enum exposes the cached-serializer accessor"
    );
}

#[test]
fn deser_ctor_appends_marker_for_value_class_field() {
    // A `@Serializable` data class with a value-class-typed field: kotlinc appends a trailing
    // `DefaultConstructorMarker` to the synthetic deserialization ctor (its value-class-param ABI
    // disambiguator), on top of the usual `SerializationConstructorMarker`.
    let Some((_file, mut ir)) =
        lower("@JvmInline value class V(val s: String)\nclass D(val v: V, val n: Int)")
    else {
        eprintln!("skipping: no stdlib jar / class outside IR subset");
        return;
    };
    let d_id = ir
        .classes
        .iter()
        .position(|c| c.fq_name().ends_with("D"))
        .expect("lowered D class present") as u32;

    let mut ctx = PluginContext::default();
    ctx.class_annotations
        .insert(d_id, vec![SERIALIZABLE_FQ.to_string()].into());
    let mut host = PluginHost::new();
    host.register(Box::new(SerializationPlugin::default()));
    host.run(&mut ir, &ctx);

    let deser = ir.classes[d_id as usize]
        .secondary_ctors
        .iter()
        .find(|sc| sc.synthetic)
        .expect("synthetic deserialization ctor synthesized");
    let last_two: Vec<Option<String>> = deser
        .params
        .iter()
        .rev()
        .take(2)
        .map(|t| t.obj_internal().map(|n| n.render()))
        .collect();
    assert_eq!(
        last_two,
        vec![
            Some("kotlin/jvm/internal/DefaultConstructorMarker".to_string()),
            Some("kotlinx/serialization/internal/SerializationConstructorMarker".to_string()),
        ],
        "deser ctor ends with SerializationConstructorMarker then DefaultConstructorMarker"
    );
}

#[test]
fn value_class_serializer_omits_write_self() {
    // A `@JvmInline value class` serializes its sole underlying value inline — kotlinc emits NO
    // `write$Self` helper for it (unlike a plain data class). krusty must match: emitting one is a
    // spurious extra member the downstream ABI gate flags.
    let Some((_file, mut ir)) = lower("@JvmInline value class V(val s: String)") else {
        eprintln!("skipping: no stdlib jar / class outside IR subset");
        return;
    };

    let v_id = ir
        .classes
        .iter()
        .position(|c| c.fq_name().ends_with("V"))
        .expect("lowered V class present") as u32;
    assert!(ir.classes[v_id as usize].is_value, "V is a value class");

    let mut ctx = PluginContext::default();
    ctx.class_annotations
        .insert(v_id, vec![SERIALIZABLE_FQ.to_string()].into());

    let mut host = PluginHost::new();
    host.register(Box::new(SerializationPlugin::default()));
    host.run(&mut ir, &ctx);

    let has_write_self = ir.classes[v_id as usize]
        .methods
        .iter()
        .any(|&f| ir.functions[f as usize].name.starts_with("write$Self"));
    assert!(
        !has_write_self,
        "value class must not get a write$Self helper"
    );
}

#[test]
fn top_level_function_registers_parameter_defaults_for_plugins() {
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
        .param_defaults(fid)
        .expect("top-level fn with defaults must register parameter defaults");
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
