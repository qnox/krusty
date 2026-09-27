//! The entry point the frontend selects for each source unit and records in the module index.

use crate::diag::DiagSink;
use crate::features::LangFeatures;
use crate::libraries::EmptySymbolSource;
use crate::source::SourceInput;

use super::*;

/// One analyzed source unit: its top-level functions in source order, each with its callable, and
/// the entry point the index recorded for it.
struct Unit {
    functions: Vec<(String, CallableId)>,
    entry: Option<ResolvedEntryPoint>,
}

impl Unit {
    fn names(&self) -> Vec<&str> {
        self.functions
            .iter()
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// The entry the test expects: the top-level function at `ordinal`, in `parameters` form.
    fn expected(
        &self,
        ordinal: usize,
        parameters: MainEntryParameters,
    ) -> Option<ResolvedEntryPoint> {
        Some(ResolvedEntryPoint {
            callable: self.functions[ordinal].1,
            parameters,
        })
    }
}

fn analyze(sources: &[&str]) -> Vec<Unit> {
    let stems = (0..sources.len())
        .map(|ordinal| format!("Entry{ordinal}"))
        .collect::<Vec<_>>();
    let inputs = sources
        .iter()
        .zip(&stems)
        .map(|(source, stem)| SourceInput::kotlin(source).with_file_stem(stem))
        .collect::<Vec<_>>();
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    (0..sources.len())
        .map(|raw| {
            let source = SourceFileId::from_raw(raw as u32);
            let mut functions = index
                .source_inventory(source)
                .iter()
                .copied()
                .filter(|declaration| {
                    index
                        .declaration_anchor(*declaration)
                        .is_some_and(|anchor| {
                            anchor.owner.is_none() && anchor.kind == DeclarationKind::Function
                        })
                })
                .map(|declaration| {
                    (
                        index.source_order(declaration).expect("source order"),
                        index
                            .declaration_name(declaration)
                            .expect("name")
                            .to_owned(),
                        index
                            .callable_for_declaration(declaration)
                            .expect("callable")
                            .id,
                    )
                })
                .collect::<Vec<_>>();
            functions.sort_by_key(|(order, ..)| *order);
            Unit {
                functions: functions
                    .into_iter()
                    .map(|(_, name, callable)| (name, callable))
                    .collect(),
                entry: index.source_entry_point(source),
            }
        })
        .collect()
}

fn single(source: &str) -> Unit {
    analyze(&[source]).pop().expect("one unit")
}

#[test]
fn parameterless_main_is_the_entry_point() {
    let unit = single("fun helper() {}\nfun main() { helper() }\n");
    assert_eq!(unit.names(), ["helper", "main"]);
    assert_eq!(unit.entry, unit.expected(1, MainEntryParameters::None));
}

#[test]
fn main_with_arguments_is_the_entry_point() {
    let unit = single("fun main(args: Array<String>) {}\n");
    assert_eq!(unit.names(), ["main"]);
    assert_eq!(unit.entry, unit.expected(0, MainEntryParameters::Arguments));
}

#[test]
fn vararg_string_main_is_the_arguments_form() {
    let unit = single("fun main(vararg args: String) {}\n");
    assert_eq!(unit.names(), ["main"]);
    assert_eq!(unit.entry, unit.expected(0, MainEntryParameters::Arguments));
}

#[test]
fn main_with_arguments_is_selected_over_a_parameterless_main_in_the_same_unit() {
    let unit = single("fun main() {}\nfun main(args: Array<String>) {}\n");
    assert_eq!(unit.names(), ["main", "main"]);
    assert_eq!(unit.entry, unit.expected(1, MainEntryParameters::Arguments));
}

#[test]
fn each_unit_records_its_own_entry_point() {
    let units = analyze(&[
        "fun main() {}\n",
        "fun other() {}\nfun main(args: Array<String>) {}\n",
    ]);
    assert_eq!(
        units[0].entry,
        units[0].expected(0, MainEntryParameters::None)
    );
    assert_eq!(
        units[1].entry,
        units[1].expected(1, MainEntryParameters::Arguments)
    );
}

#[test]
fn main_taking_a_non_array_parameter_is_not_an_entry_point() {
    let unit = single("fun main(x: Int) {}\n");
    assert_eq!(unit.names(), ["main"]);
    assert_eq!(unit.entry, None);
}

#[test]
fn main_members_of_a_class_and_an_object_are_not_entry_points() {
    let unit = single(
        "class Host { fun main() {} }\nobject Launcher { fun main(args: Array<String>) {} }\n",
    );
    assert_eq!(unit.names(), Vec::<&str>::new());
    assert_eq!(unit.entry, None);
}

#[test]
fn a_generic_main_is_not_an_entry_point() {
    assert_eq!(single("fun <T> main() {}\n").entry, None);
}

#[test]
fn an_extension_main_is_not_an_entry_point() {
    assert_eq!(single("fun String.main() {}\n").entry, None);
}

#[test]
fn a_main_returning_a_value_is_not_an_entry_point() {
    assert_eq!(single("fun main(): Int = 0\n").entry, None);
}

#[test]
fn a_unit_without_main_has_no_entry_point() {
    let unit = single("fun box(): String = \"OK\"\n");
    assert_eq!(unit.names(), ["box"]);
    assert_eq!(unit.entry, None);
}
