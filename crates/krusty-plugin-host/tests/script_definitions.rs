//! Script template definitions in the real host: a template compiled by kotlinc 2.4.20, whose
//! compilation configuration the host reads as kotlinc's `-script-templates` does. Ignored by
//! default because it downloads; CI runs it.

use std::path::{Path, PathBuf};

use krusty_plugin_host::{HostError, HostRuntime, ScriptTemplateDefinition, ScriptType};

mod support;
use support::{kotlinc, runtime, start};

/// The runtime's jar named `name`, such as `kotlin-scripting-common`.
fn jar(runtime: &HostRuntime, name: &str) -> PathBuf {
    runtime
        .compiler_classpath
        .iter()
        .find(|jar| {
            jar.file_name()
                .and_then(|file| file.to_str())
                .is_some_and(|file| file == format!("{name}-2.4.20.jar"))
        })
        .unwrap_or_else(|| panic!("the host runtime has {name}"))
        .clone()
}

/// The scripting libraries a template compiles against, with the standard library.
fn template_libraries(runtime: &HostRuntime) -> Vec<PathBuf> {
    [
        "kotlin-stdlib",
        "kotlin-scripting-common",
        "kotlin-scripting-jvm",
    ]
    .map(|name| jar(runtime, name))
    .to_vec()
}

/// Compiles `source` as `name` into a directory of its own and returns the template classpath:
/// that directory, then the libraries it compiled against.
fn compiled_template(runtime: &HostRuntime, name: &str, source: &str) -> Vec<String> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("script-template-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let sources = dir.join("src");
    std::fs::create_dir_all(&sources).unwrap();
    let file = sources.join("Template.kt");
    std::fs::write(&file, source).unwrap();
    let classes = dir.join("classes");
    let libraries = template_libraries(runtime);
    kotlinc(runtime, &libraries, &classes, &[file.as_path()], &[]);
    std::iter::once(classes.as_path())
        .chain(libraries.iter().map(PathBuf::as_path))
        .map(|entry: &Path| entry.to_string_lossy().into_owned())
        .collect()
}

fn named(name: &str) -> ScriptType {
    ScriptType {
        name: name.into(),
        nullable: false,
        arguments: Vec::new(),
    }
}

const HOST_TEMPLATE: &str = "package t\n\
\n\
import kotlin.script.experimental.annotations.KotlinScript\n\
import kotlin.script.experimental.api.*\n\
\n\
class Host\n\
class Extra\n\
\n\
@KotlinScript(fileExtension = \"host.kts\", compilationConfiguration = HostConfig::class)\n\
abstract class HostScript(val target: Host, val names: List<String>?) {\n\
\x20   constructor(name: String) : this(Host(), listOf(name))\n\
}\n\
\n\
object HostConfig : ScriptCompilationConfiguration({\n\
\x20   implicitReceivers(Host::class, Extra::class)\n\
\x20   defaultImports(\"kotlin.math.abs\", \"java.io.*\")\n\
\x20   providedProperties(\"who\" to String::class)\n\
\x20   isStandalone(false)\n\
})\n";

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn a_template_configuration_is_read_as_kotlinc_reads_it() {
    let (runtime, env) = runtime();
    let classpath = compiled_template(&runtime, "host", HOST_TEMPLATE);
    let mut host = start(&runtime, &env);
    assert_eq!(
        host.script_definition(&classpath, "t.HostScript").unwrap(),
        ScriptTemplateDefinition {
            file_extension: "host.kts".into(),
            base_class: Some(named("t.HostScript")),
            constructor_parameters: vec![
                ("target".into(), named("t.Host")),
                (
                    "names".into(),
                    ScriptType {
                        name: "kotlin.collections.List".into(),
                        nullable: true,
                        arguments: vec![named("kotlin.String")],
                    }
                ),
            ],
            implicit_receivers: vec![named("t.Host"), named("t.Extra")],
            provided_properties: vec![("who".into(), named("kotlin.String"))],
            default_imports: vec!["kotlin.math.abs".into(), "java.io.*".into()],
            is_standalone: false,
            other_keys: Vec::new(),
        }
    );
}

/// A configuration that sets what a definition's reader does not read, such as compiler options or
/// a file path pattern, names those keys, so a compiler that does not apply them can refuse the
/// template.
const OPTIONS_TEMPLATE: &str = "package t\n\
\n\
import kotlin.script.experimental.annotations.KotlinScript\n\
import kotlin.script.experimental.api.*\n\
\n\
@KotlinScript(compilationConfiguration = OptionsConfig::class, filePathPattern = \"scripts/.*\")\n\
abstract class OptionsScript\n\
\n\
object OptionsConfig : ScriptCompilationConfiguration({\n\
\x20   compilerOptions(\"-Xno-param-assertions\")\n\
})\n";

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn keys_the_reader_does_not_read_are_named() {
    let (runtime, env) = runtime();
    let classpath = compiled_template(&runtime, "options", OPTIONS_TEMPLATE);
    let mut host = start(&runtime, &env);
    assert_eq!(
        host.script_definition(&classpath, "t.OptionsScript")
            .unwrap(),
        ScriptTemplateDefinition {
            file_extension: "kts".into(),
            base_class: Some(named("t.OptionsScript")),
            constructor_parameters: Vec::new(),
            implicit_receivers: Vec::new(),
            provided_properties: Vec::new(),
            default_imports: Vec::new(),
            is_standalone: true,
            other_keys: vec!["compilerOptions".into(), "filePathPattern".into()],
        }
    );
}

#[test]
#[ignore = "downloads kotlin-compiler-embeddable 2.4.20"]
fn a_template_that_is_not_on_the_classpath_is_refused_by_name() {
    let (runtime, env) = runtime();
    let classpath = compiled_template(&runtime, "missing", HOST_TEMPLATE);
    let mut host = start(&runtime, &env);
    assert_eq!(
        host.script_definition(&classpath, "t.Missing").unwrap_err(),
        HostError::Plugin {
            message: "the script template t.Missing is not on the classpath".into()
        }
    );
}
