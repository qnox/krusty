//! The symbol each class and function of a file is emitted under.
//!
//! Every symbol a program defines shares one namespace with the runtime's, so a name is handed out
//! only once it is free of both: the runtime's own symbols are reserved first, and a repeat gets a
//! numbered suffix whose own spelling is checked in turn.

use std::collections::HashSet;

use crate::ir::IrFile;

/// A symbol-safe spelling of a Kotlin name: every non-alphanumeric character becomes `_`.
/// Symbol both files of a module use for one top-level function.
///
/// The id is the checked callable identity, so the file that defines the function and the file
/// that calls it name the same symbol without seeing each other's lowering. A suffix would break
/// that: the caller has no way to know a collision the defining file resolved locally.
pub(super) fn module_function_symbol(callable: crate::fir::CallableId) -> String {
    format!("kt_mod_{}", callable.raw())
}

/// Symbol of the getter entry point of a property one file of a module declares and another uses.
///
/// Same rule as [`module_function_symbol`]: the id is the checked property identity, so the file
/// that declares the property and the file that reads it name one symbol without seeing each
/// other's lowering. A `var`'s setter is [`module_property_setter_symbol`].
pub(super) fn module_property_getter_symbol(property: crate::fir::PropertyId) -> String {
    format!("kt_modprop_get_{}", property.raw())
}

/// Symbol of the setter entry point of a module `var`; see [`module_property_getter_symbol`].
pub(super) fn module_property_setter_symbol(property: crate::fir::PropertyId) -> String {
    format!("kt_modprop_set_{}", property.raw())
}

/// Symbol of a file's once-only top-level initializer. Both the file and any caller in the module
/// derive it from the source-file identity, the same way [`module_function_symbol`] is derived.
pub(super) fn file_init_symbol(source: crate::fir::SourceFileId) -> String {
    format!("kt_fileinit_{}", source.raw())
}

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
    // Reserved before `unique` is built: that closure borrows `taken`, so these inserts cannot
    // follow it. Top-level functions another file can name use the callable id. Methods keep the
    // class symbol the per-file namer assigns; a cross-file member call is virtual.
    let mut functions = vec![String::new(); ir.functions.len()];
    for (callable, function) in &ir.checked_callable_functions {
        let index = *function as usize;
        let Some(declared) = ir.functions.get(index) else {
            continue;
        };
        if declared.dispatch_receiver.is_some() {
            continue;
        }
        let symbol = module_function_symbol(*callable);
        taken.insert(symbol.clone());
        functions[index] = symbol;
    }
    // A property's entry points are synthesized from its identity rather than being entries in
    // `functions`. Reserving them keeps a function from being handed the same spelling.
    for property in ir
        .checked_properties
        .keys()
        .chain(ir.referenced_module_properties.keys())
    {
        taken.insert(module_property_getter_symbol(*property));
        taken.insert(module_property_setter_symbol(*property));
    }
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
    // Methods are named by their class, then everything else by the package. A slot filled above
    // is a cross-file symbol and stays.
    for (class, base) in ir.classes.iter().zip(&classes) {
        for &fid in &class.methods {
            if !functions[fid as usize].is_empty() {
                continue;
            }
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
