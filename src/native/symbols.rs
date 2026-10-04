//! The symbol each class and function of a file is emitted under.
//!
//! Every symbol a program defines shares one namespace with the runtime's, so a name is handed out
//! only once it is free of both: the runtime's own symbols are reserved first, and a repeat gets a
//! numbered suffix whose own spelling is checked in turn.

use std::collections::HashSet;

use crate::ir::IrFile;

/// A symbol-safe spelling of a Kotlin name: every non-alphanumeric character becomes `_`.
pub(super) fn c_identifier(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character);
        } else {
            out.push('_');
        }
    }
    out
}

/// Kotlin overloads share a name and C has no overloading, so a repeat gets a suffix. The suffixed
/// spelling is itself checked for collisions: a file declaring both `greet` and `greet__1` would
/// otherwise hand two functions the same symbol, and the linker would silently pick one of them.
/// Classes draw from the same pool as functions because their descriptors and constructors are
/// ordinary C objects too: a class `X` and a top-level function `type_X` must not both become
/// `kt_type_X`.
pub(super) struct Symbols {
    /// The sanitized, unique base name of each class, from which `struct kt_class_<base>`,
    /// `kt_type_<base>`, `kt_<base>__init` and the struct members `<base>_f<i>` derive.
    pub classes: Vec<String>,
    /// C symbol per IR function index.
    pub functions: Vec<String>,
}

fn class_symbol_family(base: &str) -> [String; 6] {
    [
        format!("kt_type_{base}"),
        format!("kt_vtable_{base}"),
        format!("kt_refs_{base}"),
        format!("kt_{base}__init"),
        format!("kt_singleton_{base}"),
        format!("kt_singleton_{base}_get"),
    ]
}

pub(super) fn symbols(ir: &IrFile, reserved: HashSet<String>) -> Symbols {
    // The runtime's own symbols are taken before any of the program's are handed out. A Kotlin
    // `fun cast(value: Any)` would otherwise be named `kt_cast`, which the runtime already defines,
    // and the link fails with a duplicate symbol that says nothing about the Kotlin name behind it.
    let mut taken: HashSet<String> = reserved;
    let mut unique = |base: String, family: &dyn Fn(&str) -> Vec<String>| -> String {
        let mut candidate = base.clone();
        let mut ordinal = 0;
        loop {
            let names = family(&candidate);
            if names.iter().all(|name| !taken.contains(name)) {
                taken.extend(names);
                return candidate;
            }
            ordinal += 1;
            candidate = format!("{base}__{ordinal}");
        }
    };

    // Classes first: a class reserves a whole family of names, and a function only one.
    let classes: Vec<String> = ir
        .classes
        .iter()
        .map(|class| {
            unique(c_identifier(&class.fq_name()), &|base| {
                class_symbol_family(base).to_vec()
            })
        })
        .collect();

    let package = ir
        .package
        .as_deref()
        .map(c_identifier)
        .filter(|package| !package.is_empty());
    let mut functions = vec![String::new(); ir.functions.len()];
    // Methods are named by their class, then everything else by the package.
    for (class, base) in ir.classes.iter().zip(&classes) {
        for &fid in &class.methods {
            let function = &ir.functions[fid as usize];
            let method = format!("kt_{base}_{}", c_identifier(&function.name));
            functions[fid as usize] = unique(method, &|name| vec![name.to_string()]);
        }
    }
    for (index, function) in ir.functions.iter().enumerate() {
        if !functions[index].is_empty() {
            continue;
        }
        let base = match &package {
            Some(package) => format!("kt_{package}_{}", c_identifier(&function.name)),
            None => format!("kt_{}", c_identifier(&function.name)),
        };
        functions[index] = unique(base, &|name| vec![name.to_string()]);
    }
    Symbols { classes, functions }
}
