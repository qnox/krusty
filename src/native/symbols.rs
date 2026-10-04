//! The symbol each function of a file is emitted under.
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
pub(super) struct Symbols {
    /// C symbol per IR function index.
    pub functions: Vec<String>,
}

pub(super) fn symbols(ir: &IrFile, reserved: HashSet<String>) -> Symbols {
    // The runtime's own symbols are taken before any of the program's are handed out. A Kotlin
    // `fun cast(value: Any)` would otherwise be named `kt_cast`, which the runtime already defines,
    // and the link fails with a duplicate symbol that says nothing about the Kotlin name behind it.
    let mut taken: HashSet<String> = reserved;
    let mut unique = |base: String| -> String {
        let mut candidate = base.clone();
        let mut ordinal = 0;
        while !taken.insert(candidate.clone()) {
            ordinal += 1;
            candidate = format!("{base}__{ordinal}");
        }
        candidate
    };

    let package = ir
        .package
        .as_deref()
        .map(c_identifier)
        .filter(|package| !package.is_empty());
    let functions = ir
        .functions
        .iter()
        .map(|function| {
            unique(match &package {
                Some(package) => format!("kt_{package}_{}", c_identifier(&function.name)),
                None => format!("kt_{}", c_identifier(&function.name)),
            })
        })
        .collect();
    Symbols { functions }
}
