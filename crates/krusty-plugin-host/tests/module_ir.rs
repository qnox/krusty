//! A module's IR built in the real host, checked against kotlinc 2.4.20's own: one IR plugin dumps
//! the module under kotlinc from source and under the host from krusty's IR, and the dumps must be
//! the same. Ignored by default because it downloads; CI runs it.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use krusty_plugin_host::{
    BodyIr, CallIr, CallableIdentity, CallableKind, Callee, ClassName, Classifier, Constant,
    ExpressionIr, ExpressionKind, FileIr, FunctionIr, Host, HostError, HostRuntime, ModuleIr,
    ParameterIr, SessionRequest, Span, StatementIr, TypeIdentity, ValueId, VariableIr, Visibility,
};

mod support;
use support::{kotlinc, runtime, start};

const CALLS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/module_ir/Calls.kt"
);
const SAMPLE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/module_ir/Sample.kt"
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
        .get_or_init(|| {
            let plugin = temporary("dump-plugin");
            let _ = std::fs::remove_dir_all(&plugin);
            let source = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/dump/DumpIr.kt");
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
                ("CommandLineProcessor", "fixture.dump.DumpOptions"),
                ("CompilerPluginRegistrar", "fixture.dump.DumpRegistrar"),
            ] {
                std::fs::write(
                    services.join(format!("org.jetbrains.kotlin.compiler.plugin.{service}")),
                    class,
                )
                .unwrap();
            }
            plugin
        })
        .clone()
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
    let (_, env) = support::runtime();
    let mut host = start(runtime, &env);
    host.load(
        &[dump_plugin(runtime).display().to_string()],
        &[dump_option(out)],
    )
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

fn span(start: u32, end: u32) -> Span {
    Span { start, end }
}

fn expression(start: u32, end: u32, kind: ExpressionKind) -> ExpressionIr {
    ExpressionIr {
        span: span(start, end),
        kind,
    }
}

fn get(start: u32, end: u32, value: u32) -> Option<ExpressionIr> {
    Some(expression(
        start,
        end,
        ExpressionKind::GetValue(ValueId(value)),
    ))
}

fn int(start: u32, end: u32, value: i32) -> ExpressionIr {
    expression(start, end, ExpressionKind::Constant(Constant::Int(value)))
}

fn parameter(start: u32, end: u32, name: &str, ty: TypeIdentity) -> ParameterIr {
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
    host.generate(&sample())
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(std::fs::read_to_string(out).unwrap(), expected);
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
    let module =
        krusty::hosted_plugin_ir::plugin_module_ir(analysis, &stems, paths, "main", &mut diags);
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
        let (module, diags) = krusty_ir_at(&runtime, &source_path, &[source_path.clone()]);
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
