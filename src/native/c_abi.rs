//! The C header of a native module's public top-level functions.
//!
//! A function is declared when it is public, its name has a collision-free C spelling, every
//! parameter is a primitive or `String`, and its result is one of those types or `Unit`. Anything
//! else public is named in the header and not declared, so a caller sees the refusal instead of an
//! ABI guess. `internal` and `private` functions are absent: they are not part of the ABI.

use std::collections::HashSet;

use crate::ir::IrFile;
use crate::types::{Ty, Visibility};

use super::symbols::c_identifier;

/// One line of the module header, in source order.
pub(super) enum Record {
    /// A function the header declares, and a wrapper exports under `symbol`.
    Declaration {
        function: u32,
        symbol: String,
        prototype: String,
    },
    /// A public function the header names and does not declare.
    Refusal { comment: String },
}

pub(super) fn header(module: &str, records: &[Record]) -> String {
    let guard = format!("KRUSTY_{}_H", c_identifier(module).to_ascii_uppercase());
    let mut out = String::new();
    out.push_str(
        "/* Public C ABI. A function whose name or types cannot be represented safely is named \
         here and not declared. */\n",
    );
    out.push_str(&format!("#ifndef {guard}\n#define {guard}\n"));
    out.push_str("#include <stdbool.h>\n#include <stdint.h>\n\n");
    out.push_str(
        "\
typedef int8_t kt_byte;
typedef int16_t kt_short;
typedef int32_t kt_int;
typedef int64_t kt_long;
typedef uint16_t kt_char;
typedef float kt_float;
typedef double kt_double;
typedef bool kt_boolean;
typedef void *kt_ref;

",
    );
    for record in records {
        match record {
            Record::Declaration { prototype, .. } => {
                out.push_str(prototype);
                out.push_str(";\n");
            }
            Record::Refusal { comment } => {
                out.push_str("/* ");
                out.push_str(comment);
                out.push_str(" */\n");
            }
        }
    }
    out.push_str("#endif\n");
    out
}

/// File-level functions of one file, public ones only, in source order.
pub(super) fn file_records(ir: &IrFile, symbols: &mut HashSet<String>) -> Vec<Record> {
    let mut indexes: Vec<usize> = (0..ir.functions.len())
        .filter(|index| is_file_level(ir, *index))
        .collect();
    indexes.sort_by_key(|index| {
        ir.fn_source_order
            .get(&(*index as u32))
            .copied()
            .unwrap_or(u32::MAX)
    });
    indexes
        .into_iter()
        .filter(|index| ir.method_visibility(*index as u32) == Visibility::Public)
        .map(|index| record(ir, index, symbols))
        .collect()
}

/// A function declared in the file, not a member and not a compiler-generated helper.
pub(super) fn is_file_level(ir: &IrFile, index: usize) -> bool {
    let function = &ir.functions[index];
    if function.dispatch_receiver.is_some() || function.body.is_none() {
        return false;
    }
    let id = index as u32;
    if ir.synthetic_methods.contains(&id)
        || ir.bridge_methods.contains(&id)
        || ir.class_static_local_functions.contains_key(&id)
    {
        return false;
    }
    if ir.classes.iter().any(|class| class.methods.contains(&id)) {
        return false;
    }
    true
}

fn record(ir: &IrFile, index: usize, symbols: &mut HashSet<String>) -> Record {
    let function = &ir.functions[index];
    if let Some(comment) = refusal(ir, index, &function.name, &function.params, function.ret) {
        return Record::Refusal { comment };
    }
    let tags: Vec<&str> = function.params.iter().map(|ty| type_tag(*ty)).collect();
    let symbol = symbol(ir.package.as_deref(), &function.name, &tags);
    if !is_c_global_identifier(&symbol) {
        return Record::Refusal {
            comment: format!(
                "krusty: public function `{}` has no safe C identifier",
                function.name
            ),
        };
    }
    if !symbols.insert(symbol.clone()) {
        return Record::Refusal {
            comment: format!(
                "krusty: public function `{}` would duplicate C symbol `{symbol}`",
                function.name
            ),
        };
    }
    let prototype = prototype(&symbol, ir, index as u32, &function.params, function.ret);
    Record::Declaration {
        function: index as u32,
        symbol,
        prototype,
    }
}

fn refusal(ir: &IrFile, index: usize, name: &str, params: &[Ty], ret: Ty) -> Option<String> {
    if !is_c_identifier(name) {
        return Some(format!(
            "krusty: public function `{name}` has no safe C identifier"
        ));
    }
    if ir.fn_varargs.contains_key(&(index as u32)) {
        return Some(format!(
            "krusty: the native backend does not support a public function `{name}` with a \
             vararg parameter yet"
        ));
    }
    for param in params {
        if c_type(*param).is_none() {
            return Some(format!(
                "krusty: the native backend does not support a public function `{name}` with \
                 parameter type {} yet",
                describe(*param)
            ));
        }
    }
    if ret != Ty::Unit && c_type(ret).is_none() {
        return Some(format!(
            "krusty: the native backend does not support a public function `{name}` with result \
             type {} yet",
            describe(ret)
        ));
    }
    None
}

fn symbol(package: Option<&str>, name: &str, tags: &[&str]) -> String {
    let mut symbol = match package
        .map(c_identifier)
        .filter(|package| !package.is_empty())
    {
        Some(package) => format!("{package}_{}", c_identifier(name)),
        None => c_identifier(name),
    };
    if !tags.is_empty() {
        symbol.push_str("__");
        symbol.push_str(&tags.join("_"));
    }
    symbol
}

fn prototype(symbol: &str, ir: &IrFile, function: u32, params: &[Ty], ret: Ty) -> String {
    let result = match ret {
        Ty::Unit => "void".to_string(),
        other => c_type(other).expect("a representable result").to_string(),
    };
    let arguments = if params.is_empty() {
        "void".to_string()
    } else {
        params
            .iter()
            .enumerate()
            .map(|(index, ty)| {
                format!(
                    "{} {}",
                    c_type(*ty).expect("a representable parameter"),
                    parameter_name(ir, function, index)
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!("{result} {symbol}({arguments})")
}

fn parameter_name(ir: &IrFile, function: u32, index: usize) -> String {
    ir.fn_params
        .get(&function)
        .and_then(|info| info.identities.get(index))
        .and_then(|identity| identity.source_name.clone())
        .filter(|name| is_c_identifier(name))
        .unwrap_or_else(|| format!("p{index}"))
}

fn c_type(ty: Ty) -> Option<&'static str> {
    match ty {
        Ty::Boolean => Some("kt_boolean"),
        Ty::Byte => Some("kt_byte"),
        Ty::Short => Some("kt_short"),
        Ty::Char => Some("kt_char"),
        Ty::Int => Some("kt_int"),
        Ty::Long => Some("kt_long"),
        Ty::Float => Some("kt_float"),
        Ty::Double => Some("kt_double"),
        Ty::String => Some("kt_ref"),
        _ => None,
    }
}

fn type_tag(ty: Ty) -> &'static str {
    match ty {
        Ty::Boolean => "Boolean",
        Ty::Byte => "Byte",
        Ty::Short => "Short",
        Ty::Char => "Char",
        Ty::Int => "Int",
        Ty::Long => "Long",
        Ty::Float => "Float",
        Ty::Double => "Double",
        Ty::String => "String",
        _ => unreachable!("a representable parameter has a tag"),
    }
}

fn describe(ty: Ty) -> String {
    match ty {
        Ty::Unit => "kotlin.Unit".to_string(),
        Ty::Nullable(inner) => format!("{}?", describe(*inner)),
        Ty::Fun(_) => "a function type".to_string(),
        Ty::Nothing => "kotlin.Nothing".to_string(),
        Ty::Obj(name, []) => name.render().replace('/', "."),
        Ty::Obj(name, args) => {
            let arguments = args
                .iter()
                .map(|argument| describe(*argument))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}<{arguments}>", name.render().replace('/', "."))
        }
        _ => "an unsupported type".to_string(),
    }
}

fn is_c_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
        && !matches!(
            name,
            "auto"
                | "break"
                | "case"
                | "char"
                | "const"
                | "continue"
                | "default"
                | "do"
                | "double"
                | "else"
                | "enum"
                | "extern"
                | "float"
                | "for"
                | "goto"
                | "if"
                | "inline"
                | "int"
                | "long"
                | "register"
                | "restrict"
                | "return"
                | "short"
                | "signed"
                | "sizeof"
                | "static"
                | "struct"
                | "switch"
                | "typedef"
                | "union"
                | "unsigned"
                | "void"
                | "volatile"
                | "while"
                | "_Alignas"
                | "_Alignof"
                | "_Atomic"
                | "_Bool"
                | "_Complex"
                | "_Generic"
                | "_Imaginary"
                | "_Noreturn"
                | "_Static_assert"
                | "_Thread_local"
        )
}

fn is_c_global_identifier(name: &str) -> bool {
    is_c_identifier(name) && !name.starts_with('_')
}
