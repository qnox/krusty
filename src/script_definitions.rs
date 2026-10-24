//! Script definitions: the template class a Kotlin script compiles against and the parameters its
//! constructor takes.

use crate::types::TypeName;

/// One script definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptDefinition {
    /// The template class the script class extends, as a qualified Kotlin name. `None` when the
    /// compilation's classpath does not contain it: the script class then extends nothing.
    pub template: Option<String>,
    /// The script constructor's parameters, which it passes on to the template's constructor.
    pub constructor_parameters: Vec<ScriptParameter>,
    /// The classes of the definition's implicit receivers, as qualified Kotlin names, in the
    /// definition's order. The script receives one value of each after the template's parameters,
    /// and its body reaches their members after its own and the template's.
    pub implicit_receivers: Vec<String>,
    /// Imports every script of the definition has before its own, each a qualified name that may
    /// end in `.*`.
    pub default_imports: Vec<String>,
}

/// A script constructor parameter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptParameter {
    pub name: String,
    /// The parameter's classifier, as a qualified Kotlin name. The definition owns this identity:
    /// a script's imports and declarations never rebind it.
    pub classifier: String,
    /// The classifier's type arguments, each a qualified Kotlin name.
    pub type_arguments: Vec<String>,
}

impl ScriptDefinition {
    /// kotlinc's definition for a `.kts` file that no other definition claims: the script extends
    /// `kotlin.script.templates.standard.ScriptTemplateWithArgs` and takes the command-line
    /// arguments.
    pub fn standard() -> Self {
        Self {
            template: Some("kotlin.script.templates.standard.ScriptTemplateWithArgs".to_string()),
            constructor_parameters: vec![ScriptParameter {
                name: "args".to_string(),
                classifier: "kotlin.Array".to_string(),
                type_arguments: vec!["kotlin.String".to_string()],
            }],
            implicit_receivers: Vec::new(),
            default_imports: Vec::new(),
        }
    }

    /// This definition for a compilation whose classpath may lack its template. kotlinc compiles a
    /// script whose template it cannot find as a class of its own: no supertype and no
    /// constructor parameters, so a use of a template member is an unresolved reference.
    pub fn on_classpath(self, has_classifier: impl FnOnce(TypeName) -> bool) -> Self {
        match &self.template {
            Some(template) if !has_classifier(top_level_classifier(template)) => Self {
                template: None,
                constructor_parameters: Vec::new(),
                ..self
            },
            _ => self,
        }
    }
}

/// A template is a top-level class, so each dot of its qualified name separates a package segment.
fn top_level_classifier(qualified: &str) -> TypeName {
    crate::types::type_name(&qualified.replace('.', "/"))
}
