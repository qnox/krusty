//! JavaScript functions a `wasm-js` module imports.
//!
//! The wasm-js stdlib writes host access as a function whose whole body is a `js("…")` call, as
//! `printlnImpl(message: String?): Unit = js("console.log(message)")` does. kotlinc's
//! `JsCodeCallsLowering` turns such a function into a JavaScript function the host supplies: its
//! parameters keep their source names, so the code reads them, and a body that returns the call
//! is an expression (`(message) => console.log(message)`) where a body that only makes it is a
//! block (`(message) => { … }`). krusty imports each one from the loader under `js_code`.
//!
//! A value crosses into JavaScript as kotlinc's interop adapters pass it: a number as a number,
//! and a `String` as a JavaScript string the loader reads out of the module through two exported
//! functions.

use super::encode::{Code, Function, FunctionImport, Module, ValType};

/// The import module the loader's JavaScript functions are supplied under.
const IMPORT_MODULE: &str = "js_code";

/// How a parameter of a JavaScript function crosses from the module into JavaScript.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Crossing {
    /// A number, which the JavaScript API converts itself.
    Number,
    /// A `String` (or `null`), which the loader turns into a JavaScript string.
    String,
}

/// One imported JavaScript function: its source and how each parameter reaches it.
struct JsFunction {
    code: String,
    crossings: Vec<Crossing>,
}

/// The module's JavaScript functions, and the exports that read a string out of the module.
#[derive(Default)]
pub(super) struct JsFunctions {
    functions: Vec<JsFunction>,
    /// The exported `(string) -> length` and `(string, index) -> code unit`, once a function
    /// takes a string.
    string_reader: bool,
}

/// The JavaScript source of a function whose body is `js(code)`, as kotlinc writes it.
pub(super) fn arrow_function(parameters: &[String], code: &str, returns_the_call: bool) -> String {
    if returns_the_call {
        format!("({}) => {code}", parameters.join(", "))
    } else {
        format!("({}) => {{ {code} }}", parameters.join(", "))
    }
}

impl JsFunctions {
    /// Supply the reserved function `index`, of function type `ty`, from the JavaScript `code`.
    pub(super) fn import(
        &mut self,
        module: &mut Module,
        strings: (u32, u32),
        index: u32,
        ty: u32,
        code: String,
        crossings: Vec<Crossing>,
    ) {
        if crossings.contains(&Crossing::String) && !self.string_reader {
            self.string_reader = true;
            define_string_reader(module, strings);
        }
        module.define_import(
            index,
            FunctionImport {
                module: IMPORT_MODULE.to_string(),
                name: self.functions.len().to_string(),
                ty,
            },
        );
        self.functions.push(JsFunction { code, crossings });
    }

    /// The loader's `js_code` import object, and what it declares to read strings with.
    /// `exports` names the variable that holds the instance's exports once it exists.
    pub(super) fn loader_imports(&self, exports: &str) -> String {
        if self.functions.is_empty() {
            return String::new();
        }
        let mut source = String::new();
        if self.string_reader {
            source.push_str(&format!(
                "const kotlinString = (string) => {{\n  if (string === null) return null;\n  \
                 const length = {exports}.{STRING_LENGTH}(string);\n  let text = '';\n  \
                 for (let index = 0; index < length; index++) {{\n    \
                 text += String.fromCharCode({exports}.{STRING_UNIT}(string, index));\n  }}\n  \
                 return text;\n}};\n"
            ));
        }
        source.push_str("const js_code = {\n");
        for (name, function) in self.functions.iter().enumerate() {
            let parameters = (0..function.crossings.len())
                .map(|parameter| format!("p{parameter}"))
                .collect::<Vec<_>>();
            let arguments = function
                .crossings
                .iter()
                .zip(&parameters)
                .map(|(crossing, parameter)| match crossing {
                    Crossing::Number => parameter.clone(),
                    Crossing::String => format!("kotlinString({parameter})"),
                })
                .collect::<Vec<_>>();
            source.push_str(&format!(
                "  '{name}': ({}) => ({})({}),\n",
                parameters.join(", "),
                function.code,
                arguments.join(", ")
            ));
        }
        source.push_str("};\n");
        source
    }

    /// Whether the loader supplies a `js_code` import object.
    pub(super) fn is_empty(&self) -> bool {
        self.functions.is_empty()
    }
}

const STRING_LENGTH: &str = "krusty_string_length";
const STRING_UNIT: &str = "krusty_string_unit";

/// Export the length and the code units of a string, which is `struct { chars }` (`strings` is
/// `($chars, $String)`), for the loader to read it with.
fn define_string_reader(module: &mut Module, (chars, string): (u32, u32)) {
    let length = module.declare();
    let ty = module.func_type(vec![ValType::nullable(string)], vec![ValType::I32]);
    let mut code = Code::default();
    code.local_get(0).struct_get(string, 0).array_len();
    module.define(
        length,
        Function {
            ty,
            locals: Vec::new(),
            code,
        },
    );
    module.export_function(STRING_LENGTH, length);
    let unit = module.declare();
    let ty = module.func_type(
        vec![ValType::nullable(string), ValType::I32],
        vec![ValType::I32],
    );
    let mut code = Code::default();
    code.local_get(0)
        .struct_get(string, 0)
        .local_get(1)
        .array_get_u(chars);
    module.define(
        unit,
        Function {
            ty,
            locals: Vec::new(),
            code,
        },
    );
    module.export_function(STRING_UNIT, unit);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_returned_call_is_an_expression_and_a_made_call_a_block() {
        let parameters = ["message".to_string()];
        assert_eq!(
            arrow_function(&parameters, "console.log(message)", true),
            "(message) => console.log(message)"
        );
        assert_eq!(
            arrow_function(&parameters, "console.log(message)", false),
            "(message) => { console.log(message) }"
        );
    }

    #[test]
    fn a_string_parameter_is_read_before_the_code_runs() {
        let mut module = Module::default();
        let strings = super::super::runtime::Runtime::declare_types(&mut module);
        let ty = module.func_type(vec![ValType::nullable(strings.1)], Vec::new());
        let index = module.declare();
        let mut functions = JsFunctions::default();
        functions.import(
            &mut module,
            strings,
            index,
            ty,
            "(message) => console.log(message)".to_string(),
            vec![Crossing::String],
        );
        assert_eq!(
            functions.loader_imports("exports"),
            "const kotlinString = (string) => {\n  if (string === null) return null;\n  \
             const length = exports.krusty_string_length(string);\n  let text = '';\n  \
             for (let index = 0; index < length; index++) {\n    \
             text += String.fromCharCode(exports.krusty_string_unit(string, index));\n  }\n  \
             return text;\n};\n\
             const js_code = {\n  \
             '0': (p0) => ((message) => console.log(message))(kotlinString(p0)),\n};\n"
        );
    }
}
