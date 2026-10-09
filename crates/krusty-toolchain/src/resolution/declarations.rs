//! The dependencies each module's fragments declare, read from their refined configuration, with
//! the implicit dependencies the toolchain adds (`implicit-dependencies.kt`): the Kotlin standard
//! library, the test framework, and the runtime libraries of enabled features.

use std::path::Path;

use crate::configuration::Configuration;
use crate::maven::Coordinates;
use crate::module::{ModuleHeader, ProductType};
use crate::schema::dependency_types::{
    BOM, CATALOG, EXTERNAL_MAVEN, INTERNAL, UNSCOPED_EXTERNAL_MAVEN,
};
use crate::tree::{Node, Value};

/// The version of the toolchain whose plugin API a `jvm/amper-plugin` module compiles against.
const TOOLCHAIN_VERSION: &str = "0.13.0";

/// The repository URL that stands for the local Maven repository (`SpecialMavenLocalUrl`).
const MAVEN_LOCAL: &str = "mavenLocal";

/// What a declaration depends on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A Maven artifact, or a BOM, with the description printed after it (`, implicit`).
    Maven {
        coordinates: Coordinates,
        bom: bool,
        trace: String,
    },
    /// Another module of the project, by its index.
    Module(usize),
}

/// One dependency of a fragment (`Notation`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub target: Target,
    pub compile: bool,
    pub runtime: bool,
    pub exported: bool,
}

/// A module's fragments' dependencies, and the settings resolving them depends on.
pub struct ModuleDeclarations {
    pub name: String,
    pub main: Vec<Declaration>,
    /// The test fragment's: the main fragment's declarations, then its own.
    pub test: Vec<Declaration>,
    /// The JDK version profiles are activated for (`1.8`, `11`, `25`).
    pub jdk_version: String,
    /// `group:module` pairs never resolved (`internal.excludeDependencies`).
    pub blocklist: Vec<(String, String)>,
    /// Whether the module resolves from the local Maven repository (`repositories: [mavenLocal]`).
    pub maven_local: bool,
}

/// The value at `path` below `node`.
fn value<'a>(node: &'a Node, path: &[&str]) -> Option<&'a Value> {
    let mut current = node;
    for segment in path {
        current = current.get(segment)?;
    }
    Some(&current.value)
}

fn string(node: &Node, path: &[&str]) -> Option<String> {
    match value(node, path)? {
        Value::String(text) => Some(text.clone()),
        Value::Enum(_, text) => Some(text.to_string()),
        Value::Int(number) => Some(number.to_string()),
        _ => None,
    }
}

fn enabled(node: &Node, path: &[&str]) -> bool {
    matches!(value(node, path), Some(Value::Boolean(true)))
}

/// Read every module's declarations; `modules` and `configured` are in project order and every
/// configuration is complete.
pub fn read(
    modules: &[ModuleHeader],
    configured: &[Configuration],
) -> Result<Vec<ModuleDeclarations>, String> {
    let directories: Vec<&Path> = modules
        .iter()
        .map(|module| module.file.parent().unwrap_or(Path::new("")))
        .collect();
    modules
        .iter()
        .zip(configured)
        .map(|(module, configuration)| {
            let (Some(whole), Some(main), Some(test)) = (
                &configuration.module,
                &configuration.main,
                &configuration.test,
            ) else {
                return Err(format!("module `{}` is not configured", module.name));
            };
            let settings = main.get("settings").ok_or("a fragment without settings")?;
            let jdk = string(settings, &["jvm", "jdk", "version"])
                .and_then(|version| version.parse::<u32>().ok())
                .unwrap_or(25);
            let blocklist = match value(settings, &["internal", "excludeDependencies"]) {
                Some(Value::List(items)) => items
                    .iter()
                    .filter_map(|item| match &item.value {
                        Value::String(text) => text
                            .split_once(':')
                            .map(|(group, module)| (group.to_string(), module.to_string())),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            Ok(ModuleDeclarations {
                name: module.name.clone(),
                main: fragment(main, false, module, &directories)?,
                test: fragment(test, true, module, &directories)?,
                jdk_version: if jdk <= 8 {
                    format!("1.{jdk}")
                } else {
                    jdk.to_string()
                },
                blocklist,
                maven_local: resolves_from_maven_local(whole),
            })
        })
        .collect()
}

/// Whether the module lists the local Maven repository among those it resolves from.
fn resolves_from_maven_local(module: &Node) -> bool {
    let Some(Value::List(repositories)) = value(module, &["repositories"]) else {
        return false;
    };
    repositories.iter().any(|repository| {
        string(repository, &["url"]).as_deref() == Some(MAVEN_LOCAL)
            && !matches!(value(repository, &["resolve"]), Some(Value::Boolean(false)))
    })
}

/// The declarations of one fragment, then its implicit dependencies.
fn fragment(
    node: &Node,
    test: bool,
    module: &ModuleHeader,
    directories: &[&Path],
) -> Result<Vec<Declaration>, String> {
    let mut declarations = Vec::new();
    if let Some(Value::List(items)) = node.get("dependencies").map(|items| &items.value) {
        for item in items {
            declarations.push(declaration(item, module, directories)?);
        }
    }
    let settings = node.get("settings").ok_or("a fragment without settings")?;
    let explicit: Vec<String> = declarations
        .iter()
        .filter_map(|declaration| match &declaration.target {
            Target::Maven { coordinates, .. } => Some(coordinates.key()),
            Target::Module(_) => None,
        })
        .collect();
    declarations.extend(implicit(settings, test, module.product).into_iter().filter(
        |declaration| match &declaration.target {
            Target::Maven { coordinates, .. } => !explicit.contains(&coordinates.key()),
            Target::Module(_) => true,
        },
    ));
    Ok(declarations)
}

/// A refined `dependencies` item.
fn declaration(
    item: &Node,
    module: &ModuleHeader,
    directories: &[&Path],
) -> Result<Declaration, String> {
    let Value::Mapping {
        object: Some(object),
        ..
    } = &item.value
    else {
        return Err(format!(
            "module `{}` has an unreadable dependency",
            module.name
        ));
    };
    let (compile, runtime) = match string(item, &["scope"]).as_deref() {
        Some("compile-only") => (true, false),
        Some("runtime-only") => (false, true),
        _ => (true, true),
    };
    let exported = enabled(item, &["exported"]);
    let target = if std::ptr::eq(*object, &EXTERNAL_MAVEN) {
        Target::Maven {
            coordinates: maven_coordinates(item),
            bom: false,
            trace: String::new(),
        }
    } else if std::ptr::eq(*object, &BOM) {
        let bom = item.get("bom").ok_or("a BOM without coordinates")?;
        let maven = matches!(
            &bom.value,
            Value::Mapping { object: Some(object), .. } if std::ptr::eq(*object, &UNSCOPED_EXTERNAL_MAVEN)
        );
        if !maven {
            return Err(unsupported_catalog(module));
        }
        Target::Maven {
            coordinates: maven_coordinates(bom),
            bom: true,
            trace: String::new(),
        }
    } else if std::ptr::eq(*object, &INTERNAL) {
        let Some(Value::Path(path)) = value(item, &["path"]) else {
            return Err(format!(
                "module `{}` depends on a module without a path",
                module.name
            ));
        };
        let index = directories
            .iter()
            .position(|directory| *directory == path.as_path())
            .ok_or_else(|| {
                format!(
                    "module `{}` depends on `{}`, which is not a module of the project",
                    module.name,
                    path.display()
                )
            })?;
        Target::Module(index)
    } else if std::ptr::eq(*object, &CATALOG) {
        return Err(unsupported_catalog(module));
    } else {
        return Err(format!(
            "module `{}` has an unreadable dependency",
            module.name
        ));
    };
    Ok(Declaration {
        target,
        compile,
        runtime,
        exported,
    })
}

fn unsupported_catalog(module: &ModuleHeader) -> String {
    format!(
        "module `{}` depends on a version catalog entry; krusty-toolchain does not implement version catalogs yet",
        module.name
    )
}

fn maven_coordinates(node: &Node) -> Coordinates {
    let part = |name: &str| string(node, &[name]);
    Coordinates::with_selector(
        &part("groupId").unwrap_or_default(),
        &part("artifactId").unwrap_or_default(),
        part("version").as_deref(),
        part("classifier").as_deref(),
        part("packagingType").as_deref(),
    )
}

/// An implicit dependency, compiled and run but not exported.
fn implicit_maven(
    group: &str,
    artifact: &str,
    version: &str,
    bom: bool,
    reason: Option<&str>,
) -> Declaration {
    Declaration {
        target: Target::Maven {
            coordinates: Coordinates::new(group, artifact, Some(version)),
            bom,
            trace: match reason {
                Some(reason) => format!(", implicit ({reason})"),
                None => ", implicit".to_string(),
            },
        },
        compile: true,
        runtime: true,
        exported: false,
    }
}

/// The dependencies the toolchain adds to a JVM fragment (`calculateImplicitDependencies`).
fn implicit(settings: &Node, test: bool, product: ProductType) -> Vec<Declaration> {
    let kotlin = string(settings, &["kotlin", "version"]).unwrap_or_default();
    let version = |path: &[&str]| string(settings, path).unwrap_or_default();
    let mut implicit = vec![implicit_maven(
        "org.jetbrains.kotlin",
        "kotlin-stdlib",
        &kotlin,
        false,
        None,
    )];
    if test {
        let engine = string(settings, &["junit"]).unwrap_or_default();
        let reason = format!("because the test engine is {engine}");
        implicit.push(match engine.as_str() {
            "junit-4" => implicit_maven(
                "org.jetbrains.kotlin",
                "kotlin-test-junit",
                &kotlin,
                false,
                Some(&reason),
            ),
            "junit-5" => implicit_maven(
                "org.jetbrains.kotlin",
                "kotlin-test-junit5",
                &kotlin,
                false,
                Some(&reason),
            ),
            _ => implicit_maven("org.jetbrains.kotlin", "kotlin-test", &kotlin, false, None),
        });
    }
    if enabled(settings, &["kotlin", "serialization", "enabled"]) {
        let serialization = version(&["kotlin", "serialization", "version"]);
        implicit.push(implicit_maven(
            "org.jetbrains.kotlinx",
            "kotlinx-serialization-core",
            &serialization,
            false,
            Some("because Kotlinx Serialization is enabled"),
        ));
        if let Some(format) = string(settings, &["kotlin", "serialization", "format"]) {
            if format != "none" {
                implicit.push(implicit_maven(
                    "org.jetbrains.kotlinx",
                    &format!("kotlinx-serialization-{format}"),
                    &serialization,
                    false,
                    Some(&format!("because kotlin.serialization.format={format}")),
                ));
            }
        }
    }
    if enabled(settings, &["kotlin", "dataframe", "enabled"]) {
        implicit.push(implicit_maven(
            "org.jetbrains.kotlinx",
            "dataframe-core",
            &version(&["kotlin", "dataframe", "version"]),
            false,
            Some("because Kotlin DataFrame is enabled"),
        ));
    }
    if enabled(settings, &["kotlin", "powerAssert", "enabled"]) {
        implicit.push(implicit_maven(
            "org.jetbrains.kotlin",
            "kotlin-power-assert-runtime",
            &kotlin,
            false,
            Some("because PowerAssert is enabled"),
        ));
    }
    if enabled(settings, &["lombok", "enabled"]) {
        implicit.push(implicit_maven(
            "org.projectlombok",
            "lombok",
            &version(&["lombok", "version"]),
            false,
            Some("because Lombok is enabled"),
        ));
    }
    if enabled(settings, &["compose", "enabled"]) {
        let compose = version(&["compose", "version"]);
        let reason = Some("because Compose is enabled");
        implicit.push(implicit_maven(
            "org.jetbrains.compose.runtime",
            "runtime",
            &compose,
            false,
            reason,
        ));
        implicit.push(implicit_maven(
            "org.jetbrains.compose.components",
            "components-resources",
            &compose,
            false,
            reason,
        ));
    }
    if enabled(settings, &["kotlin", "rpc", "enabled"]) {
        let rpc = version(&["kotlin", "rpc", "version"]);
        implicit.push(implicit_maven(
            "org.jetbrains.kotlinx",
            "kotlinx-rpc-core",
            &rpc,
            false,
            Some("because kotlinx.rpc is enabled"),
        ));
        if enabled(settings, &["kotlin", "rpc", "applyBom"]) {
            implicit.push(implicit_maven(
                "org.jetbrains.kotlinx",
                "kotlinx-rpc-bom",
                &rpc,
                true,
                Some("because kotlinx.rpc is enabled and kotlin.rpc.applyBom=true"),
            ));
        }
    }
    if enabled(settings, &["ktor", "enabled"]) && enabled(settings, &["ktor", "applyBom"]) {
        implicit.push(implicit_maven(
            "io.ktor",
            "ktor-bom",
            &version(&["ktor", "version"]),
            true,
            Some("because Ktor is enabled and ktor.applyBom=true"),
        ));
    }
    if enabled(settings, &["springBoot", "enabled"])
        && enabled(settings, &["springBoot", "applyBom"])
    {
        implicit.push(implicit_maven(
            "org.springframework.boot",
            "spring-boot-dependencies",
            &version(&["springBoot", "version"]),
            true,
            Some("because Spring Boot is enabled and springBoot.applyBom=true"),
        ));
    }
    if product == ProductType::AmperPlugin {
        implicit.push(implicit_maven(
            "org.jetbrains.amper",
            "amper-extensibility-api",
            TOOLCHAIN_VERSION,
            false,
            None,
        ));
    }
    implicit
}
