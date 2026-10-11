//! `settings:`, transcribed from the toolchain's `Settings` schema and its nested settings classes.
//! Properties appear in source order; printing sorts them.
//!
//! Settings that apply only to other platforms (Android, native, JavaScript) are kept with their
//! names, so a JVM module setting one is read as the toolchain reads it; their contents are not
//! modelled.

use super::dependencies::UNSCOPED_DEPENDENCIES;
use super::types::{
    property, Default, DependencyKind, Derivation, EnumType, ObjectType, Platforms, Type,
};

const STRING_LIST: Type = Type::List(&Type::String);
const STRING_MAP: Type = Type::Map(&Type::String);
const NONE: &[&str] = &[];

const fn reference(path: &'static [&'static str]) -> Default {
    Default::Reference {
        path,
        derivation: None,
    }
}

const fn derived(path: &'static [&'static str], derivation: Derivation) -> Default {
    Default::Reference {
        path,
        derivation: Some(derivation),
    }
}

pub static SETTINGS: ObjectType = ObjectType::new(
    "Settings",
    &[
        property("jvm", Type::Object(&JVM), Default::Nested),
        property("java", Type::Object(&JAVA), Default::Nested).on(Platforms::JvmAndAndroid),
        property("kotlin", Type::Object(&KOTLIN), Default::Nested),
        // Read without its schema: krusty-toolchain builds no Android modules, and on a JVM module
        // the toolchain only warns that the section has no effect.
        property("android", Type::Opaque, Default::Null).on(Platforms::Android),
        property("compose", Type::Object(&COMPOSE), Default::Nested).agnostic(),
        property("junit", Type::Enum(&JUNIT), Default::Enum("junit-5"))
            .on(Platforms::JvmAndAndroid)
            .misnomers(&["test"]),
        property("publishing", Type::Object(&PUBLISHING), Default::Nested).agnostic(),
        property("native", Type::Object(&NATIVE), Default::Null)
            .nullable()
            .on(Platforms::Native),
        property("ktor", Type::Object(&KTOR), Default::Nested),
        property("springBoot", Type::Object(&SPRING_BOOT), Default::Nested).on(Platforms::Jvm),
        property("lombok", Type::Object(&LOMBOK), Default::Nested).on(Platforms::JvmAndAndroid),
        property("internal", Type::Object(&INTERNAL), Default::Nested)
            .hidden()
            .agnostic(),
    ],
);

static NATIVE: ObjectType = ObjectType::new(
    "NativeSettings",
    &[property("entryPoint", Type::NonBlankString, Default::Null).nullable()],
);

static JS_PLAIN_OBJECTS: ObjectType = ObjectType::new(
    "JsPlainObjectsSettings",
    &[property("enabled", Type::Boolean, Default::Boolean(false)).shorthand()],
);

static JUNIT: EnumType = EnumType::new("JUnitVersion", &["junit-4", "junit-5", "none"]);

static COMPOSE: ObjectType = ObjectType::new(
    "ComposeSettings",
    &[
        property("enabled", Type::Boolean, Default::Boolean(false)).shorthand(),
        property("version", Type::NonBlankString, Default::String("1.12.1")),
        property(
            "resources",
            Type::Object(&COMPOSE_RESOURCES),
            Default::Nested,
        ),
        property(
            "experimental",
            Type::Object(&COMPOSE_EXPERIMENTAL),
            Default::Nested,
        ),
    ],
);

static COMPOSE_RESOURCES: ObjectType = ObjectType::new(
    "ComposeResourcesSettings",
    &[
        property("packageName", Type::String, Default::String("")),
        property(
            "nameOfResClass",
            Type::NonBlankString,
            Default::String("Res"),
        ),
        property("exposedAccessors", Type::Boolean, Default::Boolean(false)),
    ],
);

static COMPOSE_EXPERIMENTAL: ObjectType = ObjectType::new(
    "ComposeExperimentalSettings",
    &[property(
        "hotReload",
        Type::Object(&COMPOSE_HOT_RELOAD),
        Default::Nested,
    )
    .on(Platforms::Jvm)],
);

static COMPOSE_HOT_RELOAD: ObjectType = ObjectType::new(
    "ComposeExperimentalHotReloadSettings",
    &[property(
        "version",
        Type::NonBlankString,
        Default::String("1.2.0"),
    )],
);

static KTOR: ObjectType = ObjectType::new(
    "KtorSettings",
    &[
        property("enabled", Type::Boolean, Default::Boolean(false)).shorthand(),
        property("version", Type::NonBlankString, Default::String("3.6.0")),
        property("applyBom", Type::Boolean, Default::Boolean(true)),
    ],
);

static SPRING_BOOT: ObjectType = ObjectType::new(
    "SpringBootSettings",
    &[
        property("enabled", Type::Boolean, Default::Boolean(false)).shorthand(),
        property("version", Type::NonBlankString, Default::String("4.1.1")),
        property("applyBom", Type::Boolean, Default::Boolean(true)),
    ],
);

static LOMBOK: ObjectType = ObjectType::new(
    "LombokSettings",
    &[
        property("enabled", Type::Boolean, Default::Boolean(false)).shorthand(),
        property("version", Type::NonBlankString, Default::String("1.18.48")),
    ],
);

static INTERNAL: ObjectType = ObjectType::new(
    "InternalSettings",
    &[property(
        "excludeDependencies",
        STRING_LIST,
        Default::List(NONE),
    )],
);

// --- jvm, java ---

static JVM: ObjectType = ObjectType::new(
    "JvmSettings",
    &[
        property("jdk", Type::Object(&JDK), Default::Nested)
            .misnomers(&["provisioning"])
            .agnostic(),
        property("release", Type::Int, reference(&["jdk", "version"]))
            .nullable()
            .misnomers(&["source", "target", "apiVersion"])
            .agnostic(),
        property("mainClass", Type::MainClass, Default::Null)
            .nullable()
            .jvm_app_only(),
        property(
            "storeParameterNames",
            Type::Boolean,
            Default::Boolean(false),
        )
        .misnomers(&["parameters"])
        .agnostic(),
        property("test", Type::Object(&JVM_TEST), Default::Nested),
        property(
            "runtimeClasspathMode",
            Type::Enum(&DEPENDENCY_MODE),
            Default::Enum("jars"),
        )
        .agnostic(),
    ],
);

static DEPENDENCY_MODE: EnumType = EnumType::new("DependencyMode", &["classes", "jars"]);

static JDK: ObjectType = ObjectType::new(
    "JdkSettings",
    &[
        property("version", Type::Int, Default::Int(25))
            .misnomers(&["majorVersion", "major"])
            .agnostic(),
        property(
            "distributions",
            Type::List(&Type::Enum(&JVM_DISTRIBUTION)),
            Default::Null,
        )
        .nullable()
        .misnomers(&["vendors"])
        .agnostic(),
        property(
            "selectionMode",
            Type::Enum(&JDK_SELECTION_MODE),
            Default::Enum("auto"),
        )
        .misnomers(&["javaHome", "provisioning"])
        .agnostic(),
        property(
            "acknowledgedLicenses",
            Type::List(&Type::Enum(&JVM_DISTRIBUTION)),
            Default::List(NONE),
        )
        .misnomers(&["paid", "proprietary"])
        .agnostic(),
    ],
);

static JDK_SELECTION_MODE: EnumType =
    EnumType::new("JdkSelectionMode", &["auto", "alwaysProvision", "javaHome"]);

static JVM_DISTRIBUTION: EnumType = EnumType::new(
    "JvmDistribution",
    &[
        "temurin",
        "zulu",
        "corretto",
        "jetbrains",
        "oracleOpenJdk",
        "microsoft",
        "dragonwell",
        "liberica",
        "sapMachine",
        "semeru",
        "graalVM",
        "oracleGraalVM",
    ],
);

static JVM_TEST: ObjectType = ObjectType::new(
    "JvmTestSettings",
    &[
        property(
            "junitPlatformVersion",
            Type::NonBlankString,
            Default::String("6.1.3"),
        ),
        property("systemProperties", STRING_MAP, Default::EmptyMap),
        property("extraEnvironment", STRING_MAP, Default::EmptyMap),
        property("freeJvmArgs", STRING_LIST, Default::List(NONE)),
    ],
);

static JAVA: ObjectType = ObjectType::new(
    "JavaSettings",
    &[
        property(
            "annotationProcessing",
            Type::Object(&JAVA_ANNOTATION_PROCESSING),
            Default::Nested,
        ),
        property("freeCompilerArgs", STRING_LIST, Default::List(NONE)).misnomers(&[
            "compilation",
            "arguments",
            "options",
        ]),
        property(
            "compileIncrementally",
            Type::Boolean,
            Default::Boolean(false),
        )
        .misnomers(&["compilation", "incremental"]),
    ],
);

static JAVA_ANNOTATION_PROCESSING: ObjectType = ObjectType::new(
    "JavaAnnotationProcessingSettings",
    &[
        property("processors", UNSCOPED_DEPENDENCIES, Default::List(NONE)),
        property("processorOptions", STRING_MAP, Default::EmptyMap)
            .misnomers(&["processorSettings"]),
    ],
);

// --- kotlin ---

static KOTLIN: ObjectType = ObjectType::new(
    "KotlinSettings",
    &[
        property("version", Type::NonBlankString, Default::String("2.4.20"))
            .misnomers(&["compiler"])
            .agnostic(),
        property("languageVersion", Type::String, Default::Null)
            .nullable()
            .misnomers(&["language-version"])
            .agnostic(),
        property("apiVersion", Type::String, reference(&["languageVersion"]))
            .nullable()
            .misnomers(&["api-version", "sdkVersion", "sdk"])
            .agnostic(),
        property(
            "compileIncrementally",
            Type::Boolean,
            derived(&["version"], Derivation::KotlinIncrementalCompilation),
        )
        .misnomers(&["avoidance", "compilation"]),
        property(
            "allWarningsAsErrors",
            Type::Boolean,
            Default::Boolean(false),
        )
        .misnomers(&["Werror"]),
        property("freeCompilerArgs", STRING_LIST, Default::Null)
            .nullable()
            .misnomers(&["compilation", "arguments", "options"]),
        property("suppressWarnings", Type::Boolean, Default::Boolean(false)).misnomers(&["nowarn"]),
        property(
            "explicitApi",
            Type::Enum(&EXPLICIT_API_MODE),
            Default::Enum("disable"),
        )
        .misnomers(&["explicitApiMode"]),
        property("verbose", Type::Boolean, Default::Boolean(false)),
        property("linkerOptions", STRING_LIST, Default::Null)
            .nullable()
            .on(Platforms::Native)
            .misnomers(&["linkerOpts", "arguments"]),
        property("debug", Type::Boolean, Default::Null)
            .nullable()
            .on(Platforms::Native),
        property("optimization", Type::Boolean, Default::Null)
            .nullable()
            .on(Platforms::Native),
        property("progressiveMode", Type::Boolean, Default::Boolean(false)),
        property("languageFeatures", STRING_LIST, Default::Null)
            .nullable()
            .deprecated(
                "This kind of compiler arguments is advised against by the Kotlin team, so the \
                 convenience of this property will be removed in a future version. Use \
                 `freeCompilerArgs: [-XXLanguage:+feature]` instead if you really must.",
                true,
            ),
        property("optIns", STRING_LIST, Default::Null).nullable(),
        property("ksp", Type::Object(&KSP), Default::Nested),
        property(
            "serialization",
            Type::Object(&SERIALIZATION),
            Default::Nested,
        )
        .misnomers(&["kotlinx", "kotlinx-serialization", "kotlinx.serialization"])
        .agnostic(),
        property("noArg", Type::Object(&NO_ARG), Default::Nested).agnostic(),
        property("allOpen", Type::Object(&ALL_OPEN), Default::Nested).agnostic(),
        property("dataframe", Type::Object(&DATAFRAME), Default::Nested).agnostic(),
        property(
            "jsPlainObjects",
            Type::Object(&JS_PLAIN_OBJECTS),
            Default::Nested,
        )
        .on(Platforms::Js),
        property("powerAssert", Type::Object(&POWER_ASSERT), Default::Nested).agnostic(),
        property("rpc", Type::Object(&RPC), Default::Nested)
            .misnomers(&["kotlinx", "kotlinx-rpc", "kotlinx.rpc"])
            .agnostic(),
        property(
            "compilerPlugins",
            Type::List(&Type::Object(&THIRD_PARTY_COMPILER_PLUGIN)),
            Default::List(NONE),
        )
        .agnostic(),
    ],
);

static EXPLICIT_API_MODE: EnumType =
    EnumType::new("ExplicitApiMode", &["strict", "warning", "disable"]);

static KSP: ObjectType = ObjectType::new(
    "KspSettings",
    &[
        property("version", Type::NonBlankString, Default::String("2.3.12")).agnostic(),
        property("processors", UNSCOPED_DEPENDENCIES, Default::List(NONE)),
        property("processorOptions", STRING_MAP, Default::EmptyMap)
            .misnomers(&["processorSettings"]),
    ],
);

static SERIALIZATION: ObjectType = ObjectType::new(
    "SerializationSettings",
    &[
        property(
            "enabled",
            Type::Boolean,
            derived(&["format"], Derivation::EnabledWhenSpecified),
        )
        .shorthand(),
        property("format", Type::NonBlankString, Default::Null)
            .nullable()
            .shorthand(),
        property("version", Type::NonBlankString, Default::String("1.11.0")),
    ],
);

static NO_ARG: ObjectType = ObjectType::new(
    "NoArgSettings",
    &[
        property("enabled", Type::Boolean, Default::Boolean(false)).shorthand(),
        property("annotations", STRING_LIST, Default::Null).nullable(),
        property("invokeInitializers", Type::Boolean, Default::Boolean(false)),
        property(
            "presets",
            Type::List(&Type::Enum(&NO_ARG_PRESET)),
            Default::Null,
        )
        .nullable(),
    ],
);

static NO_ARG_PRESET: EnumType = EnumType::new("NoArgPreset", &["jpa"]);

static ALL_OPEN: ObjectType = ObjectType::new(
    "AllOpenSettings",
    &[
        property("enabled", Type::Boolean, Default::Boolean(false)).shorthand(),
        property("annotations", STRING_LIST, Default::Null).nullable(),
        property(
            "presets",
            Type::List(&Type::Enum(&ALL_OPEN_PRESET)),
            Default::Null,
        )
        .nullable(),
    ],
);

static ALL_OPEN_PRESET: EnumType =
    EnumType::new("AllOpenPreset", &["spring", "micronaut", "quarkus"]);

static DATAFRAME: ObjectType = ObjectType::new(
    "DataFrameSettings",
    &[
        property("enabled", Type::Boolean, Default::Boolean(false)).shorthand(),
        property(
            "version",
            Type::NonBlankString,
            Default::String("1.0.0-rc01"),
        ),
    ],
);

static POWER_ASSERT: ObjectType = ObjectType::new(
    "PowerAssertSettings",
    &[
        property("enabled", Type::Boolean, Default::Boolean(false)).shorthand(),
        property("functions", STRING_LIST, Default::List(&["kotlin.assert"])),
    ],
);

static RPC: ObjectType = ObjectType::new(
    "KotlinxRpcSettings",
    &[
        property("enabled", Type::Boolean, Default::Boolean(false)).shorthand(),
        property("applyBom", Type::Boolean, Default::Boolean(true)),
        property("version", Type::NonBlankString, Default::String("0.10.4")),
        property(
            "annotationTypeSafetyEnabled",
            Type::Boolean,
            Default::Boolean(true),
        ),
    ],
);

static THIRD_PARTY_COMPILER_PLUGIN: ObjectType = ObjectType::new(
    "ThirdPartyCompilerPlugin",
    &[
        property("id", Type::NonBlankString, Default::Required),
        property(
            "dependency",
            Type::Dependency(DependencyKind::UnscopedExternal),
            Default::Required,
        ),
        property("options", STRING_MAP, Default::EmptyMap),
    ],
);

// --- publishing ---

static PUBLISHING: ObjectType = ObjectType::new(
    "PublishingSettings",
    &[
        property("enabled", Type::Boolean, Default::Boolean(false)),
        property("group", Type::NonBlankString, Default::Null).nullable(),
        property("version", Type::NonBlankString, Default::Null).nullable(),
        property("artifactId", Type::NonBlankString, reference(&["name"]))
            .nullable()
            .misnomers(&["name"]),
        property("name", Type::String, Default::Null)
            .nullable()
            .deprecated("Obsolete, use `artifactId` instead", false),
        property("pom", Type::Object(&POM), Default::Nested),
        property("signArtifacts", Type::Boolean, Default::Boolean(false)),
        property("publishSources", Type::Boolean, Default::Boolean(false)),
        property(
            "checksums",
            Type::List(&Type::Enum(&CHECKSUM)),
            Default::List(&["md5", "sha1"]),
        ),
        property(
            "mavenCentral",
            Type::Object(&MAVEN_CENTRAL),
            Default::Nested,
        ),
    ],
);

static CHECKSUM: EnumType = EnumType::new("Checksum", &["md5", "sha1", "sha256", "sha512"]);

static MAVEN_CENTRAL: ObjectType = ObjectType::new(
    "MavenCentralSettings",
    &[
        property("enabled", Type::Boolean, Default::Boolean(false)).shorthand(),
        property(
            "publishingMode",
            Type::Enum(&PUBLISHING_MODE),
            Default::Enum("manual"),
        ),
    ],
);

static PUBLISHING_MODE: EnumType = EnumType::new("PublishingMode", &["manual", "auto"]);

static POM: ObjectType = ObjectType::new(
    "PomSettings",
    &[
        property("name", Type::NonBlankString, Default::Null).nullable(),
        property("description", Type::String, Default::Null).nullable(),
        property("url", Type::NonBlankString, Default::Null).nullable(),
        property(
            "licenses",
            Type::List(&Type::Object(&LICENSE)),
            Default::List(NONE),
        ),
        property("scm", Type::Object(&SCM), Default::Nested),
        property(
            "developers",
            Type::List(&Type::Object(&DEVELOPER)),
            Default::List(NONE),
        ),
    ],
);

static LICENSE: ObjectType = ObjectType::new(
    "LicenseInfo",
    &[
        property("name", Type::NonBlankString, Default::Required),
        property("url", Type::NonBlankString, Default::Required),
    ],
);

static SCM: ObjectType = ObjectType::new(
    "ScmInfo",
    &[
        property("url", Type::NonBlankString, Default::Null)
            .nullable()
            .shorthand(),
        property(
            "connection",
            Type::String,
            derived(&["url"], Derivation::ScmConnection),
        )
        .nullable(),
        property(
            "developerConnection",
            Type::String,
            derived(&["url"], Derivation::ScmDeveloperConnection),
        )
        .nullable(),
    ],
);

static DEVELOPER: ObjectType = ObjectType::new(
    "DeveloperInfo",
    &[
        property("id", Type::String, Default::Null).nullable(),
        property("name", Type::NonBlankString, Default::Required).shorthand(),
        property("url", Type::String, Default::Null).nullable(),
        property("email", Type::String, Default::Null).nullable(),
        property("organization", Type::String, Default::Null).nullable(),
        property("organizationUrl", Type::String, Default::Null).nullable(),
    ],
);
