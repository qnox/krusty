//! `-script-templates`: kotlinc builds each template's compilation configuration by running the
//! template's code, so krusty reads it in the plugin host's JVM and compiles a script against the
//! definition that configuration describes.

use std::path::{Path, PathBuf};

use krusty::script_definitions::{ScriptDefinition, ScriptParameter};
use krusty_plugin_host::{
    provision, Curl, Environment, Host, Pins, Request, ScriptTemplateDefinition, ScriptType,
};

/// The definitions of a compilation's script templates, in the order the command line names them.
pub struct ScriptTemplates {
    /// Each template's file extension, such as `host.kts`, and its definition.
    definitions: Vec<(String, ScriptDefinition)>,
}

impl ScriptTemplates {
    /// Reads `templates`, classes on `classpath`, in a plugin host at `kotlin_version`. A template
    /// whose configuration sets anything krusty does not apply is refused by name.
    pub fn read(
        templates: &[String],
        classpath: &[PathBuf],
        kotlin_version: &str,
    ) -> Result<Self, String> {
        let env = Environment::from_process()
            .ok_or("cannot read script templates: no cache directory for the plugin host")?;
        let pins = Pins::builtin(&env.platform);
        let request = Request {
            kotlin_version,
            reason: "the -script-templates compilation configurations",
            min_java: 8,
        };
        let runtime = provision(&request, &env, &pins, &Curl, &mut std::io::stderr())
            .map_err(|error| error.to_string())?;
        let mut host = Host::start(&runtime, &env, &mut std::io::stderr())
            .map_err(|error| error.to_string())?;
        let classpath = classpath
            .iter()
            .map(|entry| entry.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let definitions = templates
            .iter()
            .map(|template| {
                let read = host
                    .script_definition(&classpath, template)
                    .map_err(|error| error.to_string())?;
                Ok((read.file_extension.clone(), definition(template, read)?))
            })
            .collect::<Result<_, String>>()?;
        Ok(Self { definitions })
    }

    /// The definition of the first template that claims `path`, the one whose file extension ends
    /// its file name. kotlinc compiles a script no template claims against its standard definition.
    pub fn for_source(&self, path: &Path) -> Option<&ScriptDefinition> {
        let name = path.file_name()?.to_str()?;
        self.definitions
            .iter()
            .find(|(extension, _)| {
                name.strip_suffix(extension.as_str())
                    .is_some_and(|stem| stem.ends_with('.'))
            })
            .map(|(_, definition)| definition)
    }
}

/// The definition `read` describes for `template`, or why krusty cannot compile its scripts.
fn definition(template: &str, read: ScriptTemplateDefinition) -> Result<ScriptDefinition, String> {
    let unsupported = |what: String| {
        format!("krusty cannot compile scripts of the template {template} yet: {what}")
    };
    if !read.other_keys.is_empty() {
        return Err(unsupported(format!(
            "its compilation configuration sets {}",
            read.other_keys.join(", ")
        )));
    }
    if !read.provided_properties.is_empty() {
        return Err(unsupported(
            "its compilation configuration provides properties".into(),
        ));
    }
    let base_class = read
        .base_class
        .ok_or_else(|| unsupported("its compilation configuration names no base class".into()))?;
    let constructor_parameters = read
        .constructor_parameters
        .into_iter()
        .map(|(name, parameter)| {
            script_parameter(name, parameter)
                .map_err(|ty| unsupported(format!("its base class takes a parameter of type {ty}")))
        })
        .collect::<Result<_, _>>()?;
    let implicit_receivers = read
        .implicit_receivers
        .into_iter()
        .map(|receiver| {
            class_name(&receiver).map(str::to_owned).ok_or_else(|| {
                unsupported(format!(
                    "it has an implicit receiver of type {}",
                    spelled(&receiver)
                ))
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(ScriptDefinition {
        template: Some(base_class.name),
        constructor_parameters,
        implicit_receivers,
        default_imports: read.default_imports,
    })
}

/// The script constructor parameter `name` of type `parameter`: a class whose type arguments, if
/// any, are classes themselves. Any other type comes back spelled out.
fn script_parameter(name: String, parameter: ScriptType) -> Result<ScriptParameter, String> {
    let type_arguments = parameter
        .arguments
        .iter()
        .map(|argument| class_name(argument).map(str::to_owned))
        .collect::<Option<_>>()
        .filter(|_| !parameter.nullable)
        .ok_or_else(|| spelled(&parameter))?;
    Ok(ScriptParameter {
        name,
        classifier: parameter.name,
        type_arguments,
    })
}

/// The name of `ty` when it is a non-null class without type arguments.
fn class_name(ty: &ScriptType) -> Option<&str> {
    (!ty.nullable && ty.arguments.is_empty()).then_some(ty.name.as_str())
}

/// `ty` as Kotlin source spells it.
fn spelled(ty: &ScriptType) -> String {
    let mut text = ty.name.clone();
    if !ty.arguments.is_empty() {
        let arguments = ty.arguments.iter().map(spelled).collect::<Vec<_>>();
        text.push_str(&format!("<{}>", arguments.join(", ")));
    }
    if ty.nullable {
        text.push('?');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(name: &str) -> ScriptType {
        ScriptType {
            name: name.into(),
            nullable: false,
            arguments: Vec::new(),
        }
    }

    fn read(constructor_parameters: Vec<(String, ScriptType)>) -> ScriptTemplateDefinition {
        ScriptTemplateDefinition {
            file_extension: "host.kts".into(),
            base_class: Some(class("t.HostScript")),
            constructor_parameters,
            implicit_receivers: vec![class("t.Host")],
            provided_properties: Vec::new(),
            default_imports: vec!["kotlin.math.abs".into()],
            is_standalone: false,
            other_keys: Vec::new(),
        }
    }

    #[test]
    fn a_template_definition_becomes_the_definition_its_scripts_compile_against() {
        let mut names = class("kotlin.Array");
        names.arguments.push(class("kotlin.String"));
        let definition = definition(
            "t.HostScript",
            read(vec![
                ("target".into(), class("t.Host")),
                ("names".into(), names),
            ]),
        )
        .unwrap();
        assert_eq!(
            definition,
            ScriptDefinition {
                template: Some("t.HostScript".into()),
                constructor_parameters: vec![
                    ScriptParameter {
                        name: "target".into(),
                        classifier: "t.Host".into(),
                        type_arguments: Vec::new(),
                    },
                    ScriptParameter {
                        name: "names".into(),
                        classifier: "kotlin.Array".into(),
                        type_arguments: vec!["kotlin.String".into()],
                    },
                ],
                implicit_receivers: vec!["t.Host".into()],
                default_imports: vec!["kotlin.math.abs".into()],
            }
        );
    }

    #[test]
    fn what_a_definition_cannot_say_refuses_the_template_by_name() {
        let mut names = class("kotlin.collections.List");
        names.nullable = true;
        names.arguments.push(class("kotlin.String"));
        assert_eq!(
            definition("t.HostScript", read(vec![("names".into(), names)])),
            Err(
                "krusty cannot compile scripts of the template t.HostScript yet: its base class \
                 takes a parameter of type kotlin.collections.List<kotlin.String>?"
                    .into()
            )
        );
        let mut options = read(Vec::new());
        options.other_keys = vec!["compilerOptions".into(), "filePathPattern".into()];
        assert_eq!(
            definition("t.HostScript", options),
            Err(
                "krusty cannot compile scripts of the template t.HostScript yet: its compilation \
                 configuration sets compilerOptions, filePathPattern"
                    .into()
            )
        );
        let mut provided = read(Vec::new());
        provided.provided_properties = vec![("who".into(), class("kotlin.String"))];
        assert_eq!(
            definition("t.HostScript", provided),
            Err(
                "krusty cannot compile scripts of the template t.HostScript yet: its compilation \
                 configuration provides properties"
                    .into()
            )
        );
    }

    #[test]
    fn a_script_belongs_to_the_first_template_whose_extension_ends_its_name() {
        let named = |template: &str| ScriptDefinition {
            template: Some(template.into()),
            constructor_parameters: Vec::new(),
            implicit_receivers: Vec::new(),
            default_imports: Vec::new(),
        };
        let templates = ScriptTemplates {
            definitions: vec![
                ("host.kts".into(), named("t.Host")),
                ("kts".into(), named("t.Any")),
            ],
        };
        let template_of = |path: &str| {
            templates
                .for_source(Path::new(path))
                .and_then(|definition| definition.template.as_deref())
        };
        assert_eq!(template_of("scripts/greet.host.kts"), Some("t.Host"));
        assert_eq!(template_of("ghost.kts"), Some("t.Any"));
        assert_eq!(template_of("host.kts"), Some("t.Any"));
    }
}
