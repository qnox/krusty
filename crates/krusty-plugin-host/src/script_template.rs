//! A script template's definition, as the plugin host reads it from the template's compilation
//! configuration.

/// What a `-script-templates` template's compilation configuration defines, read in the host as
/// kotlinc reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptTemplateDefinition {
    /// The file name suffix of the scripts the template defines, such as `host.kts`.
    pub file_extension: String,
    /// The class every script of the template extends.
    pub base_class: Option<ScriptType>,
    /// The classes of the scripts' implicit receivers, in the configuration's order.
    pub implicit_receivers: Vec<ScriptType>,
    /// The properties each script is given, by name, in the configuration's order.
    pub provided_properties: Vec<(String, ScriptType)>,
    /// The imports every script has before its own.
    pub default_imports: Vec<String>,
    pub is_standalone: bool,
    /// The names of the configuration's other keys, sorted: what a compiler that reads only the
    /// keys above would leave out.
    pub other_keys: Vec<String>,
}

/// A type a script configuration names, spelled as the scripting API spells it: a qualified class
/// name with `.` between packages and nested classes alike.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptType {
    pub name: String,
    pub nullable: bool,
}
