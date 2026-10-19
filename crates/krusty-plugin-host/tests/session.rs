//! A compilation's session in the real host: kotlinc 2.4.20's JVM frontend and fir2ir over a
//! classpath, and declarations found in it by identity. Ignored by default because it downloads; CI
//! runs it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use krusty_plugin_host::{
    provision, CallableIdentity, CallableKind, ClassName, Classifier, Curl, Environment, Host,
    HostError, HostRuntime, Pins, Request, SessionRequest, TypeArgument, TypeIdentity,
    TypeParameterOwner,
};

mod support;
use support::{kotlinc, runtime, start};

fn stdlib(runtime: &HostRuntime) -> PathBuf {
    runtime
        .compiler_classpath
        .iter()
        .find(|jar| jar.ends_with("kotlin-stdlib-2.4.20.jar"))
        .unwrap()
        .clone()
}

fn session(runtime: &HostRuntime, classpath: Vec<PathBuf>) -> SessionRequest {
    SessionRequest {
        classpath,
        jdk_home: runtime.java_home.clone(),
        module_name: "main".into(),
        language_version: "2.4".into(),
        api_version: "2.4".into(),
        jvm_target: "1.8".into(),
    }
}

fn opened(runtime: &HostRuntime, classpath: Vec<PathBuf>) -> Host {
    let (_, env) = support::runtime();
    let mut host = start(runtime, &env);
    host.open_session(&session(runtime, classpath))
        .unwrap_or_else(|error| panic!("{error}"));
    host
}

fn class(package: &str, classes: &[&str]) -> ClassName {
    ClassName {
        package: package.into(),
        classes: classes.iter().map(|class| class.to_string()).collect(),
    }
}

fn of(package: &str, name: &str, arguments: Vec<TypeArgument>, nullable: bool) -> TypeIdentity {
    TypeIdentity {
        classifier: Classifier::Class {
            class: class(package, &[name]),
            arguments,
        },
        nullable,
        annotations: Vec::new(),
    }
}

fn kotlin(name: &str) -> TypeIdentity {
    of("kotlin", name, Vec::new(), false)
}

fn type_parameter(owner: TypeParameterOwner, index: u32) -> TypeIdentity {
    TypeIdentity {
        classifier: Classifier::TypeParameter { owner, index },
        nullable: false,
        annotations: Vec::new(),
    }
}

fn function(package: &str, name: &str, value_parameters: Vec<TypeIdentity>) -> CallableIdentity {
    CallableIdentity {
        kind: CallableKind::Function { name: name.into() },
        package: package.into(),
        classes: Vec::new(),
        type_parameters: 0,
        context_parameters: Vec::new(),
        extension_receiver: None,
        value_parameters,
    }
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn declarations_on_the_classpath_resolve_by_their_exact_signature() {
    let (runtime, _) = runtime();
    let mut host = opened(&runtime, vec![stdlib(&runtime)]);
    let identities = [
        function(
            "kotlin.io",
            "println",
            vec![of("kotlin", "Any", Vec::new(), true)],
        ),
        CallableIdentity {
            type_parameters: 1,
            ..function(
                "kotlin.collections",
                "listOf",
                vec![of(
                    "kotlin",
                    "Array",
                    vec![TypeArgument::Out(type_parameter(
                        TypeParameterOwner::Callable,
                        0,
                    ))],
                    false,
                )],
            )
        },
        CallableIdentity {
            extension_receiver: Some(kotlin("String")),
            ..function("kotlin.text", "trim", Vec::new())
        },
        CallableIdentity {
            kind: CallableKind::Property {
                name: "length".into(),
            },
            classes: vec!["String".into()],
            ..function("kotlin", "", Vec::new())
        },
        CallableIdentity {
            kind: CallableKind::Constructor,
            classes: vec!["Pair".into()],
            ..function(
                "kotlin",
                "",
                vec![
                    type_parameter(TypeParameterOwner::Class(class("kotlin", &["Pair"])), 0),
                    type_parameter(TypeParameterOwner::Class(class("kotlin", &["Pair"])), 1),
                ],
            )
        },
    ];
    assert_eq!(
        host.resolve(&identities).unwrap(),
        [
            "FUN IR_EXTERNAL_DECLARATION_STUB name:println visibility:public modality:FINAL <> \
             (message:kotlin.Any?) returnType:kotlin.Unit [inline]",
            "FUN IR_EXTERNAL_DECLARATION_STUB name:listOf visibility:public modality:FINAL <T> \
             (elements:kotlin.Array<out T of kotlin.collections.listOf>) \
             returnType:kotlin.collections.List<T of kotlin.collections.listOf>",
            "FUN IR_EXTERNAL_DECLARATION_STUB name:trim visibility:public modality:FINAL <> \
             (<this>:kotlin.String) returnType:kotlin.String [inline]",
            "PROPERTY IR_EXTERNAL_DECLARATION_STUB name:length visibility:public modality:OPEN [val]",
            "CONSTRUCTOR IR_EXTERNAL_DECLARATION_STUB visibility:public <> (first:A of kotlin.Pair, \
             second:B of kotlin.Pair) returnType:kotlin.Pair<A of kotlin.Pair, B of kotlin.Pair> \
             [primary]",
        ]
    );
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn an_identity_that_names_no_declaration_fails_by_name() {
    let (runtime, _) = runtime();
    let mut host = opened(&runtime, vec![stdlib(&runtime)]);
    let missing = function("kotlin.io", "println", vec![kotlin("Int"), kotlin("Int")]);
    assert_eq!(
        host.resolve(&[missing]).unwrap_err(),
        HostError::Plugin {
            message: "fun kotlin.io.println(kotlin.Int, kotlin.Int) names no declaration on the \
                      session's classpath"
                .into()
        }
    );
}

/// `inner.Outer<T>.Inner<U>`, compiled once: its members name `T` of the outer class.
fn inner_classes(runtime: &HostRuntime) -> PathBuf {
    static OUT: OnceLock<PathBuf> = OnceLock::new();
    OUT.get_or_init(|| {
        let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("plugin-host-inner-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        let source =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/inner/Outer.kt");
        kotlinc(runtime, &[stdlib(runtime)], &out, &[source.as_path()], &[]);
        out
    })
    .clone()
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn an_inner_class_member_names_its_outer_class_type_parameter_by_that_class() {
    let (runtime, _) = runtime();
    let mut host = opened(&runtime, vec![stdlib(&runtime), inner_classes(&runtime)]);
    let outer = || type_parameter(TypeParameterOwner::Class(class("inner", &["Outer"])), 0);
    let inner = || {
        type_parameter(
            TypeParameterOwner::Class(class("inner", &["Outer", "Inner"])),
            0,
        )
    };
    let member = CallableIdentity {
        classes: vec!["Outer".into(), "Inner".into()],
        ..function("inner", "use", vec![outer(), inner()])
    };
    let constructor = CallableIdentity {
        kind: CallableKind::Constructor,
        ..member.clone()
    };
    assert_eq!(
        host.resolve(&[member.clone(), constructor]).unwrap(),
        [
            "FUN IR_EXTERNAL_DECLARATION_STUB name:use visibility:public modality:FINAL <> \
             (<this>:inner.Outer.Inner<U of inner.Outer.Inner, T of inner.Outer>, \
             value:T of inner.Outer, other:U of inner.Outer.Inner) returnType:kotlin.Unit",
            "CONSTRUCTOR IR_EXTERNAL_DECLARATION_STUB visibility:public <> \
             (<this>:inner.Outer<T of inner.Outer>, first:T of inner.Outer, \
             second:U of inner.Outer.Inner) \
             returnType:inner.Outer.Inner<U of inner.Outer.Inner, T of inner.Outer> [primary]",
        ]
    );
    // The outer class's `T` is not the inner class's own first type parameter.
    let misowned = CallableIdentity {
        value_parameters: vec![
            type_parameter(
                TypeParameterOwner::Class(class("inner", &["Outer", "Inner"])),
                0,
            ),
            inner(),
        ],
        ..member
    };
    assert_eq!(
        host.resolve(&[misowned]).unwrap_err(),
        HostError::Plugin {
            message: "fun inner.Outer.Inner.use(type parameter 0 of inner.Outer.Inner, type \
                      parameter 0 of inner.Outer.Inner) names no declaration on the session's \
                      classpath"
                .into()
        }
    );
}

/// Two jars that each declare `duplicate.twice()`, in files of different names.
fn duplicates(runtime: &HostRuntime) -> Vec<PathBuf> {
    static JARS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    JARS.get_or_init(|| {
        ["First", "Second"]
            .iter()
            .map(|name| {
                let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
                    "plugin-host-duplicate-{name}-{}",
                    std::process::id()
                ));
                let _ = std::fs::remove_dir_all(&out);
                let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join(format!("tests/fixtures/duplicate/{name}.kt"));
                kotlinc(runtime, &[stdlib(runtime)], &out, &[source.as_path()], &[]);
                out
            })
            .collect()
    })
    .clone()
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn an_identity_that_names_two_declarations_fails_by_name() {
    let (runtime, _) = runtime();
    let mut classpath = vec![stdlib(&runtime)];
    classpath.extend(duplicates(&runtime));
    let mut host = opened(&runtime, classpath);
    assert_eq!(
        host.resolve(&[function("duplicate", "twice", Vec::new())])
            .unwrap_err(),
        HostError::Plugin {
            message: "fun duplicate.twice() names 2 declarations on the session's classpath".into()
        }
    );
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn a_session_kotlinc_refuses_fails_with_kotlincs_message() {
    let (runtime, env) = runtime();
    let mut host = start(&runtime, &env);
    let request = SessionRequest {
        jdk_home: Path::new(env!("CARGO_TARGET_TMPDIR")).join("no-such-jdk"),
        ..session(&runtime, vec![stdlib(&runtime)])
    };
    assert_eq!(
        host.open_session(&request).unwrap_err(),
        HostError::Plugin {
            message: format!(
                "error: JDK home directory does not exist: {}",
                request.jdk_home.display()
            )
        }
    );
    let println = function("kotlin.io", "println", Vec::new());
    assert_eq!(
        host.resolve(std::slice::from_ref(&println)).unwrap_err(),
        HostError::Plugin {
            message: "no session is open".into()
        }
    );
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn a_closed_session_resolves_nothing() {
    let (runtime, _) = runtime();
    let mut host = opened(&runtime, vec![stdlib(&runtime)]);
    let println = function("kotlin.io", "println", Vec::new());
    assert_eq!(
        host.resolve(std::slice::from_ref(&println)).unwrap().len(),
        1
    );
    host.close_session().unwrap();
    assert_eq!(
        host.resolve(std::slice::from_ref(&println)).unwrap_err(),
        HostError::Plugin {
            message: "no session is open".into()
        }
    );
    host.close_session().unwrap();
}

/// Every Kotlin version krusty supports provisions, compiles the adapter, starts a host and opens a
/// session.
#[test]
#[ignore = "downloads kotlin-compiler-embeddable for every supported Kotlin version"]
fn every_supported_kotlin_version_starts_a_host_and_opens_a_session() {
    let env = Environment::from_process().expect("a cache directory");
    let versions = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../kotlin-versions"
    ))
    .unwrap();
    let versions: Vec<&str> = versions
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| line.split_whitespace().next().unwrap())
        .collect();
    assert!(!versions.is_empty());
    let pins = Pins::builtin(&env.platform);
    for version in versions {
        let request = Request {
            kotlin_version: version,
            reason: "the plugin host test",
            min_java: 8,
        };
        let runtime = provision(&request, &env, &pins, &Curl, &mut std::io::stderr())
            .unwrap_or_else(|error| panic!("{version}: {error}"));
        let mut host = Host::start(&runtime, &env, &mut std::io::stderr())
            .unwrap_or_else(|error| panic!("{version}: {error}"));
        assert_eq!(host.load(&[], &[]), Ok(Vec::new()), "{version}");
        let stdlib = runtime
            .compiler_classpath
            .iter()
            .find(|jar| jar.ends_with(format!("kotlin-stdlib-{version}.jar")))
            .unwrap()
            .clone();
        host.open_session(&session(&runtime, vec![stdlib]))
            .unwrap_or_else(|error| panic!("{version}: {error}"));
        let println = function(
            "kotlin.io",
            "println",
            vec![of("kotlin", "Any", Vec::new(), true)],
        );
        assert_eq!(
            host.resolve(&[println]).unwrap_or_else(|error| panic!("{version}: {error}")),
            ["FUN IR_EXTERNAL_DECLARATION_STUB name:println visibility:public modality:FINAL <> \
              (message:kotlin.Any?) returnType:kotlin.Unit [inline]"],
            "{version}"
        );
    }
}
