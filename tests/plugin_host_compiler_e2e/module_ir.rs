//! A module's IR built in the real host, checked against kotlinc 2.4.20's own: one IR plugin dumps
//! the module under kotlinc from source and under the host from krusty's IR, and the dumps must be
//! the same. Ignored by default because it downloads; CI runs it.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use krusty_plugin_api::ir::{
    BodyIr, CallIr, CallableIdentity, CallableKind, Callee, ClassName, Classifier, Constant,
    ExpressionIr, ExpressionKind, FileIr, FunctionIr, ModuleIr, ParameterIr, Span, StatementIr,
    TypeIdentity, ValueId, VariableIr, Visibility,
};
use krusty_plugin_api::ModuleIrPlugin;
use krusty_plugin_host::{Host, HostError, HostRuntime, SessionRequest};

#[path = "../../crates/krusty-plugin-host/tests/support/mod.rs"]
mod support;
use support::{kotlinc, runtime, start};

const CALLS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/crates/krusty-plugin-host/tests/fixtures/module_ir/Calls.kt"
);
const SAMPLE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/crates/krusty-plugin-host/tests/fixtures/module_ir/Sample.kt"
);

fn stdlib(runtime: &HostRuntime) -> PathBuf {
    runtime
        .compiler_classpath
        .iter()
        .find(|jar| jar.ends_with("kotlin-stdlib-2.4.20.jar"))
        .unwrap()
        .clone()
}

fn temporary(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("plugin-host-{name}-{}", std::process::id()))
}

/// The dump plugin's classes, with the services kotlinc's `-Xplugin` finds it by, in a directory.
fn dump_plugin(runtime: &HostRuntime) -> PathBuf {
    static PLUGIN: OnceLock<PathBuf> = OnceLock::new();
    PLUGIN
        .get_or_init(|| fixture_plugin(runtime, "dump", "dump/DumpIr.kt", "fixture.dump.Dump"))
        .clone()
}

/// The rewrite plugin's classes, which change the module they run over.
fn rewrite_plugin(runtime: &HostRuntime) -> PathBuf {
    static PLUGIN: OnceLock<PathBuf> = OnceLock::new();
    PLUGIN
        .get_or_init(|| {
            fixture_plugin(
                runtime,
                "rewrite",
                "rewrite/RewriteIr.kt",
                "fixture.rewrite.Rewrite",
            )
        })
        .clone()
}

/// The fixture plugin `source` (under the host's test fixtures) compiled into a directory, with the
/// services kotlinc's `-Xplugin` finds `<prefix>Options` and `<prefix>Registrar` by.
fn fixture_plugin(runtime: &HostRuntime, name: &str, source: &str, prefix: &str) -> PathBuf {
    let plugin = temporary(&format!("{name}-plugin"));
    let _ = std::fs::remove_dir_all(&plugin);
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/krusty-plugin-host/tests/fixtures")
        .join(source);
    kotlinc(
        runtime,
        &runtime.compiler_classpath,
        &plugin,
        &[source.as_ref()],
        &[],
    );
    let services = plugin.join("META-INF/services");
    std::fs::create_dir_all(&services).unwrap();
    for (service, class) in [
        ("CommandLineProcessor", "Options"),
        ("CompilerPluginRegistrar", "Registrar"),
    ] {
        std::fs::write(
            services.join(format!("org.jetbrains.kotlin.compiler.plugin.{service}")),
            format!("{prefix}{class}"),
        )
        .unwrap();
    }
    plugin
}

fn dump_option(out: &Path) -> String {
    format!("plugin:fixture.dump:out={}", out.display())
}

/// What the dump plugin writes when kotlinc compiles `source` itself.
fn kotlinc_dump(runtime: &HostRuntime, source: &str) -> String {
    let stem = Path::new(source).file_stem().unwrap().to_string_lossy();
    let out = temporary(&format!("kotlinc-dump-{stem}.txt"));
    let classes = temporary(&format!("kotlinc-dump-classes-{stem}"));
    let plugin = format!("-Xplugin={}", dump_plugin(runtime).display());
    let option = dump_option(&out);
    kotlinc(
        runtime,
        &[stdlib(runtime)],
        &classes,
        &[source.as_ref()],
        &[OsStr::new(&plugin), OsStr::new("-P"), OsStr::new(&option)],
    );
    std::fs::read_to_string(out).unwrap()
}

/// A host with the dump plugin loaded, writing to `out`, and a session open over the stdlib.
fn host_with_dump(runtime: &HostRuntime, out: &Path) -> Host {
    host_with(runtime, &[dump_plugin(runtime)], &[dump_option(out)])
}

/// A host with the plugin classpath `plugins` loaded with `options`, and a session open over the
/// stdlib.
fn host_with(runtime: &HostRuntime, plugins: &[PathBuf], options: &[String]) -> Host {
    let (_, env) = support::runtime();
    let mut host = start(runtime, &env);
    let entries: Vec<String> = plugins
        .iter()
        .map(|plugin| plugin.display().to_string())
        .collect();
    host.load(&entries, options)
        .unwrap_or_else(|error| panic!("{error}"));
    host.open_session(&SessionRequest {
        classpath: vec![stdlib(runtime)],
        jdk_home: runtime.java_home.clone(),
        module_name: "main".into(),
        language_version: "2.4".into(),
        api_version: "2.4".into(),
        jvm_target: "1.8".into(),
    })
    .unwrap_or_else(|error| panic!("{error}"));
    host
}

fn kotlin(name: &str, nullable: bool) -> TypeIdentity {
    TypeIdentity {
        classifier: Classifier::Class {
            class: ClassName {
                package: "kotlin".into(),
                classes: vec![name.into()],
            },
            arguments: Vec::new(),
        },
        nullable,
        annotations: Vec::new(),
    }
}

fn span(start: i32, end: i32) -> Span {
    Span { start, end }
}

fn expression(start: i32, end: i32, kind: ExpressionKind) -> ExpressionIr {
    ExpressionIr {
        span: span(start, end),
        kind,
    }
}

fn get(start: i32, end: i32, value: u32) -> Option<ExpressionIr> {
    Some(expression(
        start,
        end,
        ExpressionKind::GetValue(ValueId(value)),
    ))
}

fn int(start: i32, end: i32, value: i32) -> ExpressionIr {
    expression(start, end, ExpressionKind::Constant(Constant::Int(value)))
}

fn parameter(start: i32, end: i32, name: &str, ty: TypeIdentity) -> ParameterIr {
    ParameterIr {
        span: span(start, end),
        name: name.into(),
        ty,
    }
}

/// `tests/fixtures/module_ir/Sample.kt` as krusty spells its IR, with kotlinc's offsets.
fn sample() -> ModuleIr {
    let twice = FunctionIr {
        span: span(16, 77),
        name: "twice".into(),
        visibility: Visibility::Public,
        parameters: vec![parameter(26, 32, "x", kotlin("Int", false))],
        return_type: kotlin("Int", false),
        body: BodyIr {
            span: span(39, 77),
            statements: vec![
                StatementIr::Variable(VariableIr {
                    span: span(45, 54),
                    name: "y".into(),
                    ty: kotlin("Int", false),
                    mutable: false,
                    initializer: get(53, 54, 0),
                }),
                StatementIr::Expression(expression(
                    59,
                    75,
                    ExpressionKind::Return(Box::new(expression(
                        66,
                        75,
                        ExpressionKind::Call(Box::new(CallIr {
                            callee: Callee::Module {
                                file: 0,
                                function: 1,
                            },
                            ty: kotlin("Int", false),
                            type_arguments: Vec::new(),
                            arguments: vec![get(70, 71, 1), get(73, 74, 1)],
                        })),
                    ))),
                )),
            ],
        },
    };
    let add = FunctionIr {
        span: span(79, 111),
        name: "add".into(),
        visibility: Visibility::Public,
        parameters: vec![
            parameter(87, 93, "a", kotlin("Int", false)),
            parameter(95, 101, "b", kotlin("Int", false)),
        ],
        return_type: kotlin("Int", false),
        // An expression body: kotlinc returns its expression from a block body at its end.
        body: BodyIr {
            span: span(110, 111),
            statements: vec![StatementIr::Expression(expression(
                111,
                111,
                ExpressionKind::Return(Box::new(get(110, 111, 0).unwrap())),
            ))],
        },
    };
    let println = CallableIdentity {
        kind: CallableKind::Function {
            name: "println".into(),
        },
        package: "kotlin.io".into(),
        classes: Vec::new(),
        type_parameters: 0,
        context_parameters: Vec::new(),
        extension_receiver: None,
        value_parameters: vec![kotlin("Any", true)],
    };
    let greet = FunctionIr {
        span: span(113, 182),
        name: "greet".into(),
        visibility: Visibility::Public,
        parameters: vec![parameter(123, 135, "name", kotlin("String", false))],
        return_type: kotlin("Unit", false),
        body: BodyIr {
            span: span(137, 182),
            statements: vec![
                StatementIr::Expression(expression(
                    143,
                    156,
                    ExpressionKind::Call(Box::new(CallIr {
                        callee: Callee::External(Box::new(println)),
                        ty: kotlin("Unit", false),
                        type_arguments: Vec::new(),
                        arguments: vec![get(151, 155, 0)],
                    })),
                )),
                StatementIr::Variable(VariableIr {
                    span: span(161, 170),
                    name: "n".into(),
                    ty: kotlin("Int", false),
                    mutable: true,
                    initializer: Some(int(169, 170, 1)),
                }),
                StatementIr::Expression(expression(
                    175,
                    180,
                    ExpressionKind::SetValue {
                        value: ValueId(1),
                        to: Box::new(int(179, 180, 2)),
                    },
                )),
            ],
        },
    };
    ModuleIr {
        files: vec![FileIr {
            path: SAMPLE.into(),
            package: "sample".into(),
            text: std::fs::read_to_string(SAMPLE).unwrap(),
            functions: vec![twice, add, greet],
        }],
    }
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn the_host_builds_a_modules_ir_as_kotlinc_builds_it_from_source() {
    let (runtime, _) = runtime();
    let expected = kotlinc_dump(&runtime, SAMPLE);
    let out = temporary("host-dump.txt");
    let mut host = host_with_dump(&runtime, &out);
    let returned = host
        .generate(&sample())
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(std::fs::read_to_string(out).unwrap(), expected);
    // The dump plugin changes nothing, so the host returns the module it was sent.
    assert_eq!(returned, sample());
}

/// `sample()` as the rewrite plugin leaves it: each Int constant one more, and each function
/// starting with a call of `println` with its name, made without a source position.
fn rewritten_sample() -> ModuleIr {
    let println = CallableIdentity {
        kind: CallableKind::Function {
            name: "println".into(),
        },
        package: "kotlin.io".into(),
        classes: Vec::new(),
        type_parameters: 0,
        context_parameters: Vec::new(),
        extension_receiver: None,
        value_parameters: vec![kotlin("Any", true)],
    };
    let mut module = sample();
    for function in &mut module.files[0].functions {
        let name = ExpressionIr {
            span: Span::UNDEFINED,
            kind: ExpressionKind::Constant(Constant::String(function.name.clone())),
        };
        function.body.statements.insert(
            0,
            StatementIr::Expression(ExpressionIr {
                span: Span::UNDEFINED,
                kind: ExpressionKind::Call(Box::new(CallIr {
                    callee: Callee::External(Box::new(println.clone())),
                    ty: kotlin("Unit", false),
                    type_arguments: Vec::new(),
                    arguments: vec![Some(name)],
                })),
            }),
        );
    }
    let greet = &mut module.files[0].functions[2];
    let StatementIr::Variable(n) = &mut greet.body.statements[2] else {
        unreachable!()
    };
    n.initializer = Some(int(169, 170, 2));
    let StatementIr::Expression(set) = &mut greet.body.statements[3] else {
        unreachable!()
    };
    set.kind = ExpressionKind::SetValue {
        value: ValueId(1),
        to: Box::new(int(179, 180, 3)),
    };
    module
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn the_host_returns_the_module_its_plugins_leave() {
    let (runtime, _) = runtime();
    let mut host = host_with(&runtime, &[rewrite_plugin(&runtime)], &[]);
    let returned = host
        .generate(&sample())
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(returned, rewritten_sample());
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn the_host_is_a_module_ir_plugin_that_returns_the_module_its_plugins_leave() {
    let (runtime, _) = runtime();
    let mut host = host_with(&runtime, &[rewrite_plugin(&runtime)], &[]);
    let plugin: &mut dyn ModuleIrPlugin = &mut host;
    let returned = plugin
        .transform(&sample())
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(returned, rewritten_sample());
}

/// IR a plugin leaves that the plugin IR cannot carry back refuses the module by what it is, rather
/// than returning the module without it.
#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn ir_a_plugin_leaves_that_the_plugin_ir_does_not_cover_refuses_the_module() {
    assert_eq!(
        rewrite_refusal("branch"),
        "a plugin produced an expression, which krusty's plugin IR does not cover yet: WHEN type=kotlin.Int origin=null"
    );
}

/// A default value is not part of the plugin IR's parameter, so the module is refused rather than
/// returned with the parameter's default dropped.
#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn a_parameter_default_a_plugin_adds_refuses_the_module() {
    assert_eq!(
        rewrite_refusal("default"),
        "a plugin produced a parameter with a default value, which krusty's plugin IR does not cover yet: VALUE_PARAMETER kind:Regular name:x index:0 type:kotlin.Int"
    );
}

/// A callable identity names a function, never a property, so a call of an accessor is refused
/// rather than returned as a call of a function that does not exist.
#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn a_property_accessor_call_a_plugin_adds_refuses_the_module() {
    assert_eq!(
        rewrite_refusal("accessor"),
        "a plugin produced a call of a property accessor, which krusty's plugin IR does not cover yet: CALL 'public open fun <get-length> (): kotlin.Int declared in kotlin.String' type=kotlin.Int origin=null"
    );
}

/// The plugin IR's function has no modifiers or annotations, so a plugin that adds either is
/// refused rather than returned without them.
#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn a_function_modifier_or_annotation_a_plugin_adds_refuses_the_module() {
    assert_eq!(
        rewrite_refusal("inline"),
        "a plugin produced a function with the inline modifier, which krusty's plugin IR does not cover yet: FUN name:twice visibility:public modality:FINAL <> (x:kotlin.Int) returnType:kotlin.Int [inline]"
    );
    assert_eq!(
        rewrite_refusal("annotated"),
        "a plugin produced an annotated function, which krusty's plugin IR does not cover yet: FUN name:twice visibility:public modality:FINAL <> (x:kotlin.Int) returnType:kotlin.Int"
    );
}

/// A type annotation travels as its class alone, so one with an argument is refused rather than
/// returned as the same annotation with another argument.
#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn a_type_annotation_argument_a_plugin_adds_refuses_the_module() {
    assert_eq!(
        rewrite_refusal("typeannotation"),
        "krusty's plugin IR does not cover the type annotation CONSTRUCTOR_CALL 'public constructor <init> (suppress: kotlin.Boolean) [primary] declared in kotlin.jvm.JvmSuppressWildcards' type=kotlin.jvm.JvmSuppressWildcards origin=null yet"
    );
}

/// The wire carries UTF-8, so a string constant with an unpaired surrogate is refused rather than
/// returned as a different string.
#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn a_string_with_an_unpaired_surrogate_a_plugin_adds_refuses_the_module() {
    assert_eq!(
        rewrite_refusal("surrogate"),
        "a plugin produced a string constant with an unpaired surrogate, which krusty's plugin IR does not cover yet: CONST String type=kotlin.String value=\"\\uD800\""
    );
}

/// The message the host refuses the sample's module with under the rewrite plugin in `mode`.
fn rewrite_refusal(mode: &str) -> String {
    let (runtime, _) = runtime();
    let mut host = host_with(
        &runtime,
        &[rewrite_plugin(&runtime)],
        &[format!("plugin:fixture.rewrite:mode={mode}")],
    );
    match host.generate(&sample()).unwrap_err() {
        HostError::Plugin { message } => message,
        other => panic!("mode={mode}: {other:?}"),
    }
}

/// `source`'s plugin IR as krusty's compiler builds it, against the stdlib and the session's JDK.
fn krusty_ir(runtime: &HostRuntime, source: &str) -> ModuleIr {
    let (module, diags) = krusty_ir_at(runtime, source, &[source.to_string()]);
    let messages: Vec<String> = diags.diags.iter().map(|diag| diag.msg.clone()).collect();
    module.unwrap_or_else(|| panic!("krusty refused {source}: {messages:?}"))
}

/// krusty's IR of `source`, analyzed alone, with `paths` as the inputs' paths.
fn krusty_ir_at(
    runtime: &HostRuntime,
    source: &str,
    paths: &[String],
) -> (Option<ModuleIr>, krusty::diag::DiagSink) {
    let text = std::fs::read_to_string(source).unwrap();
    let mut diags = krusty::diag::DiagSink::new();
    let stem = Path::new(source)
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let inputs = [krusty::source::SourceInput::kotlin(&text).with_file_stem(&stem)];
    let stems = [stem.clone()];
    let jdk = runtime.java_home.join("lib/modules");
    assert!(
        jdk.is_file(),
        "{} is not a JDK's module image",
        jdk.display()
    );
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(vec![
        stdlib(runtime),
        jdk,
    ]));
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath).expect("the JVM provider"),
    );
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        krusty::frontend::PlatformProvider::jvm(platform),
        &krusty::features::LangFeatures::from_source(&text),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diags,
    );
    let module = krusty::plugin_ir::plugin_module_ir(analysis, &stems, paths, "main", &mut diags);
    (module, diags)
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn krusty_builds_the_ir_kotlinc_builds_from_source() {
    let (runtime, _) = runtime();
    let module = krusty_ir(&runtime, SAMPLE);
    assert_eq!(module, sample());
    let expected = kotlinc_dump(&runtime, SAMPLE);
    let out = temporary("krusty-dump.txt");
    let mut host = host_with_dump(&runtime, &out);
    host.generate(&module)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(std::fs::read_to_string(out).unwrap(), expected);
}

/// Visibilities, each kind of constant, an extension, a member and a generic call with an explicit
/// type argument, and offsets past characters that take two UTF-8 bytes.
#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn krusty_builds_the_ir_of_calls_and_constants_as_kotlinc_does() {
    let (runtime, _) = runtime();
    let module = krusty_ir(&runtime, CALLS);
    let expected = kotlinc_dump(&runtime, CALLS);
    let out = temporary("krusty-calls-dump.txt");
    let mut host = host_with_dump(&runtime, &out);
    host.generate(&module)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(std::fs::read_to_string(out).unwrap(), expected);
}

/// A file written with `\r\n` line breaks and one lone `\r`, which kotlinc reads as `\n`s: its
/// offsets, lines and columns are kotlinc's.
#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn krusty_measures_a_file_with_carriage_returns_as_kotlinc_reads_it() {
    let (runtime, _) = runtime();
    let text = std::fs::read_to_string(CALLS)
        .unwrap()
        .replace('\n', "\r\n")
        .replacen("\r\n", "\r", 3);
    assert!(text.contains("\r\n") && text.contains("\r\r"));
    let directory = temporary("carriage-returns");
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("CarriageReturns.kt");
    std::fs::write(&source, text).unwrap();
    let source = source.to_str().unwrap();
    let module = krusty_ir(&runtime, source);
    let expected = kotlinc_dump(&runtime, source);
    let out = temporary("krusty-carriage-returns-dump.txt");
    let mut host = host_with_dump(&runtime, &out);
    host.generate(&module)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(std::fs::read_to_string(out).unwrap(), expected);
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn ir_the_host_cannot_build_is_refused_and_leaves_the_session_able_to_build_its_module() {
    let (runtime, _) = runtime();
    let out = temporary("refused-dump.txt");
    let _ = std::fs::remove_file(&out);
    let mut host = host_with_dump(&runtime, &out);
    // `twice` calls `add` with one argument too few.
    let mut wrong = sample();
    let StatementIr::Expression(statement) = &mut wrong.files[0].functions[0].body.statements[1]
    else {
        unreachable!()
    };
    let ExpressionKind::Return(value) = &mut statement.kind else {
        unreachable!()
    };
    let ExpressionKind::Call(call) = &mut value.kind else {
        unreachable!()
    };
    call.arguments.pop();
    assert_eq!(
        host.generate(&wrong).unwrap_err(),
        HostError::Plugin {
            message: "a call to add passes 1 arguments to its 2 parameters".into()
        }
    );
    assert!(!out.exists());
    host.generate(&sample())
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(out.exists());
    assert_eq!(
        host.generate(&sample()).unwrap_err(),
        HostError::Plugin {
            message: "the session's module is already built; a session builds one module".into()
        }
    );
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn a_module_is_built_only_in_an_open_session() {
    let (runtime, env) = runtime();
    let mut host = start(&runtime, &env);
    assert_eq!(
        host.generate(&sample()).unwrap_err(),
        HostError::Plugin {
            message: "no session is open".into()
        }
    );
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn a_module_whose_paths_do_not_name_every_source_is_refused() {
    let (runtime, _) = runtime();
    let (module, diags) = krusty_ir_at(&runtime, SAMPLE, &[]);
    assert_eq!(module, None);
    let diagnostics: Vec<(u32, u32, u32, &str)> = diags
        .diags
        .iter()
        .map(|diag| (diag.file, diag.span.lo, diag.span.hi, diag.msg.as_str()))
        .collect();
    assert_eq!(
        diagnostics,
        [(
            0,
            0,
            0,
            "compiler plugins need the path of every source: 0 paths were given for 1 sources"
        )]
    );
}

/// A function or parameter with something kotlinc's IR records that krusty's plugin IR does not
/// carry yet is refused where it is written, not built without it.
#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn a_function_with_what_the_plugin_ir_does_not_carry_is_refused_where_it_is_written() {
    let (runtime, _) = runtime();
    // Each source, the text the refusal spans and what it names.
    let cases = [
        (
            "@Deprecated(\"old\") fun f() {}\n",
            "fun f() {}",
            "the annotations of `f`",
        ),
        (
            "suspend fun f() {}\n",
            "fun f() {}",
            "the `suspend` modifier of `f`",
        ),
        (
            "tailrec fun f(n: Int): Int { return n }\n",
            "fun f(n: Int): Int { return n }",
            "the `tailrec` modifier of `f`",
        ),
        (
            "fun f(vararg xs: Int) {}\n",
            "vararg xs: Int",
            "the `vararg` modifier of the parameter `xs` of `f`",
        ),
        (
            "fun f(@Suppress(\"x\") n: Int) {}\n",
            "@Suppress(\"x\") n: Int",
            "the annotations of the parameter `n` of `f`",
        ),
        (
            "fun f(n: Int = 1) {}\n",
            "n: Int = 1",
            "the default value of the parameter `n` of `f`",
        ),
        (
            "fun f(n: @Suppress(\"x\") Int) {}\n",
            "@Suppress",
            "an annotated type in `f`",
        ),
        (
            "fun f(): @Suppress(\"x\") Int { return 1 }\n",
            "@Suppress",
            "an annotated type in `f`",
        ),
        (
            "fun f(xs: List<@Suppress(\"x\") Int>) {}\n",
            "@Suppress",
            "an annotated type in `f`",
        ),
        (
            "fun f() { val n: @Suppress(\"x\") Int = 1 }\n",
            "@Suppress",
            "an annotated type in `f`",
        ),
        (
            "fun f() { emptyList<@Suppress(\"x\") Int>() }\n",
            "@Suppress",
            "an annotated type in `f`",
        ),
    ];
    let directory = temporary("refused-shapes");
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("Refused.kt");
    let source_path = source.to_str().unwrap().to_string();
    for (text, spanned, what) in cases {
        std::fs::write(&source, text).unwrap();
        let (module, diags) =
            krusty_ir_at(&runtime, &source_path, std::slice::from_ref(&source_path));
        assert_eq!(module, None, "{text}");
        let lo = text.find(spanned).unwrap() as u32;
        let diagnostics: Vec<(u32, u32, u32, &str)> = diags
            .diags
            .iter()
            .map(|diag| (diag.file, diag.span.lo, diag.span.hi, diag.msg.as_str()))
            .collect();
        let message = format!(
            "compiler plugins cannot run on this code yet: krusty's plugin IR does not cover {what}"
        );
        assert_eq!(
            diagnostics,
            [(0, lo, lo + spanned.len() as u32, message.as_str())],
            "{text}"
        );
    }
}
