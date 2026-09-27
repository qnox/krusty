//! Common lowering maps the entry point the frontend recorded for a file to the function that
//! realizes it. Which function is the entry is the frontend's decision (`fir::entry_point_tests`).

use super::tests::lower_single_source;
use crate::fir::MainEntryParameters;
use crate::ir::{IrEntryPoint, IrFile};

/// The published entry point, and every package function in source order with its `FunId`.
fn entry(source: &str) -> (Option<IrEntryPoint>, Vec<(String, u32)>) {
    let ir: IrFile = lower_single_source(source, "EntryPoint");
    let functions = ir
        .package_functions
        .iter()
        .map(|function| (function.name.clone(), function.function))
        .collect();
    (ir.entry_point, functions)
}

fn function(functions: &[(String, u32)], ordinal: usize) -> u32 {
    functions[ordinal].1
}

fn names(functions: &[(String, u32)]) -> Vec<&str> {
    functions.iter().map(|(name, _)| name.as_str()).collect()
}

#[test]
fn a_recorded_parameterless_main_maps_to_its_function() {
    let (entry, functions) = entry("fun helper() {}\nfun main() { helper() }\n");
    assert_eq!(names(&functions), ["helper", "main"]);
    assert_eq!(
        entry,
        Some(IrEntryPoint {
            function: function(&functions, 1),
            parameters: MainEntryParameters::None,
        })
    );
}

#[test]
fn a_recorded_main_with_arguments_maps_to_its_function() {
    let (entry, functions) = entry("fun main(args: Array<String>) {}\n");
    assert_eq!(names(&functions), ["main"]);
    assert_eq!(
        entry,
        Some(IrEntryPoint {
            function: function(&functions, 0),
            parameters: MainEntryParameters::Arguments,
        })
    );
}

#[test]
fn the_recorded_arguments_form_maps_to_its_own_function_beside_a_parameterless_main() {
    let (entry, functions) = entry("fun main() {}\nfun main(args: Array<String>) {}\n");
    assert_eq!(names(&functions), ["main", "main"]);
    assert_eq!(
        entry,
        Some(IrEntryPoint {
            function: function(&functions, 1),
            parameters: MainEntryParameters::Arguments,
        })
    );
}

#[test]
fn a_file_without_a_recorded_entry_point_has_none() {
    let (entry, functions) = entry("fun box(): String = \"OK\"\n");
    assert_eq!(names(&functions), ["box"]);
    assert_eq!(entry, None);
}
