//! A stable JSON rendering of the resolved [`ProjectModel`].
//!
//! This is the contract the out-of-process parity harness (`scripts/parity-scan.py`) consumes: it
//! asks `krusty-lsp model <root>` once for a whole worktree and then drives the compiler per module
//! from the result, instead of re-deriving source roots and classpaths itself. Keeping the shape in
//! one place — and pinning it with tests — means the harness never has to guess at what the language
//! server actually resolved.

use serde_json::{json, Map, Value};

use super::model::{Module, ModuleOutput, ProjectModel, SourceRoot, SourceRootKind};

/// The `schema` field emitted with every dump. Bump when a field changes meaning (not when one is
/// added — the harness tolerates unknown keys).
pub const SCHEMA_VERSION: u32 = 1;

fn path_value(path: &std::path::Path) -> Value {
    Value::String(path.to_string_lossy().into_owned())
}

fn source_root_json(root: &SourceRoot) -> Value {
    json!({
        "path": path_value(&root.path),
        "kind": match root.kind {
            SourceRootKind::Source => "source",
            SourceRootKind::Test => "test",
        },
        "generated": root.generated,
        "package_prefix": root.package_prefix,
    })
}

fn output_json(output: &ModuleOutput) -> Value {
    json!({
        "kind": match output {
            ModuleOutput::Classes(_) => "classes",
            ModuleOutput::Location(_) => "location",
        },
        "path": path_value(output.path()),
    })
}

fn module_json(module: &Module) -> Value {
    let mut object = Map::new();
    object.insert(
        "id".to_string(),
        module
            .id
            .as_ref()
            .map_or(Value::Null, |id| Value::String(id.as_str().to_string())),
    );
    object.insert(
        "name".to_string(),
        Value::String(module.display_name.clone()),
    );
    object.insert(
        "base_directory".to_string(),
        path_value(&module.base_directory),
    );
    object.insert(
        "source_roots".to_string(),
        Value::Array(module.source_roots.iter().map(source_root_json).collect()),
    );
    object.insert(
        "classpath".to_string(),
        Value::Array(module.classpath.iter().map(|p| path_value(p)).collect()),
    );
    object.insert(
        "outputs".to_string(),
        Value::Array(module.outputs.iter().map(output_json).collect()),
    );
    object.insert(
        "depends_on".to_string(),
        Value::Array(
            module
                .depends_on
                .iter()
                .map(|id| Value::String(id.as_str().to_string()))
                .collect(),
        ),
    );
    object.insert(
        "friend_paths".to_string(),
        Value::Array(module.friend_paths.iter().map(|p| path_value(p)).collect()),
    );
    object.insert(
        "jvm_target".to_string(),
        module
            .jvm_target
            .as_ref()
            .map_or(Value::Null, |target| Value::String(target.clone())),
    );
    object.insert(
        "kotlinc_args".to_string(),
        Value::Array(
            module
                .kotlinc_args
                .iter()
                .map(|argument| Value::String(argument.clone()))
                .collect(),
        ),
    );
    Value::Object(object)
}

/// Render `model` as the harness-facing JSON document.
pub fn model_json(model: &ProjectModel) -> Value {
    json!({
        "schema": SCHEMA_VERSION,
        "root": path_value(&model.root),
        "provider": model.kind.as_str(),
        "jdk_home": model.jdk_home.as_deref().map_or(Value::Null, path_value),
        "modules": Value::Array(model.modules.iter().map(module_json).collect()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::model::{ModuleId, ProviderKind};
    use std::path::PathBuf;

    fn sample() -> ProjectModel {
        let mut module = Module::new(
            ModuleId::raw("intellij.platform.util"),
            "/repo/platform/util",
        );
        module.source_roots = vec![
            SourceRoot::source("/repo/platform/util/src"),
            SourceRoot::test("/repo/platform/util/testSrc"),
        ];
        module.classpath = vec![PathBuf::from("/repo/lib/guava.jar")];
        module.outputs = vec![ModuleOutput::classes("/repo/out/util")];
        module.depends_on = vec![ModuleId::raw("intellij.platform.core")];
        module.friend_paths = vec![PathBuf::from("/repo/out/utilMain")];
        module.jvm_target = Some("17".to_string());
        module.kotlinc_args = vec!["-Xjvm-default=all".to_string()];
        ProjectModel {
            root: PathBuf::from("/repo"),
            kind: ProviderKind::Jps,
            jdk_home: Some(PathBuf::from("/jdk")),
            modules: vec![module],
        }
    }

    #[test]
    fn renders_every_field_the_harness_reads() {
        let value = model_json(&sample());
        assert_eq!(value["schema"], SCHEMA_VERSION);
        assert_eq!(value["root"], "/repo");
        assert_eq!(value["provider"], "jps");
        assert_eq!(value["jdk_home"], "/jdk");
        let module = &value["modules"][0];
        assert_eq!(module["id"], "intellij.platform.util");
        assert_eq!(module["name"], "intellij.platform.util");
        assert_eq!(module["base_directory"], "/repo/platform/util");
        assert_eq!(module["source_roots"][0]["path"], "/repo/platform/util/src");
        assert_eq!(module["source_roots"][0]["kind"], "source");
        assert_eq!(module["source_roots"][0]["generated"], false);
        assert_eq!(module["source_roots"][1]["kind"], "test");
        assert_eq!(module["classpath"][0], "/repo/lib/guava.jar");
        assert_eq!(module["outputs"][0]["kind"], "classes");
        assert_eq!(module["outputs"][0]["path"], "/repo/out/util");
        assert_eq!(module["depends_on"][0], "intellij.platform.core");
        assert_eq!(module["friend_paths"][0], "/repo/out/utilMain");
        assert_eq!(module["jvm_target"], "17");
        assert_eq!(module["kotlinc_args"][0], "-Xjvm-default=all");
    }

    #[test]
    fn absent_optionals_render_as_null_not_missing() {
        let mut model = sample();
        model.jdk_home = None;
        model.modules[0].jvm_target = None;
        model.modules[0].id = None;
        let value = model_json(&model);
        assert!(value["jdk_home"].is_null());
        assert!(value["modules"][0]["jvm_target"].is_null());
        assert!(value["modules"][0]["id"].is_null());
    }

    #[test]
    fn a_location_output_is_distinguishable_from_a_classes_output() {
        let mut model = sample();
        model.modules[0].outputs = vec![ModuleOutput::location("/repo/out")];
        let value = model_json(&model);
        assert_eq!(value["modules"][0]["outputs"][0]["kind"], "location");
    }
}
