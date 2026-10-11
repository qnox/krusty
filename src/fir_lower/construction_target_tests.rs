//! Every checked construction of a module class names the constructor the checker selected
//! (`IrFile::construction_targets`): its place among the class's constructors, taken from the
//! selected declaration. The contract is that a backend maps the ordinal to the primary or to
//! `secondary_ctors[n - 1]` instead of choosing between constructors by their parameter types. These
//! tests pin that common-IR record; they do not run a backend over it.

use super::tests::{lower_single_source, lower_source_from_set};
use crate::ir::{for_each_child, IrConstructorAccess, IrConstructorTarget, IrExpr, IrFile};
use crate::types::{type_name, Ty};

/// `Label(Derived())` and `Label(Derived(), 3)` fit both the primary and the first secondary
/// constructor, whose `count` defaults differ; the checker picks the primary as the more specific.
/// Every class is declared here, so the selection depends on no library.
const SOURCE: &str = "open class Base
class Derived : Base()
class Tag(val size: Int)
class Label(val base: Derived, val count: Int = 1) {
    constructor(base: Base, count: Int = 2) : this(Derived(), count)
    constructor(base: Base, tag: Tag) : this(Derived(), tag.size)
}
fun primaryWithDefault(): Label = Label(Derived())
fun primaryExplicit(): Label = Label(Derived(), 3)
fun secondaryWithDefault(): Label = Label(Base())
fun secondaryExplicit(): Label = Label(Base(), 4)
fun secondaryTagged(): Label = Label(Base(), Tag(5))
fun taggedReference(): (Base, Tag) -> Label = ::Label
";

fn body(ir: &IrFile, function: &str) -> u32 {
    let declaration = ir
        .package_functions
        .iter()
        .find(|candidate| candidate.name == function)
        .unwrap_or_else(|| panic!("package function {function}"));
    ir.functions[declaration.function as usize]
        .body
        .expect("a declared function has a body")
}

/// Each `Label` construction in `function`'s body: its selected constructor and the source-value
/// parameters it leaves to their defaults.
fn label_constructions(ir: &IrFile, function: &str) -> Vec<(IrConstructorTarget, Vec<u32>)> {
    constructions_under(ir, body(ir, function))
}

fn constructions_under(ir: &IrFile, root: u32) -> Vec<(IrConstructorTarget, Vec<u32>)> {
    let mut pending = vec![root];
    let mut constructions = Vec::new();
    while let Some(expression) = pending.pop() {
        if let IrExpr::New {
            internal, defaults, ..
        } = ir.expr(expression)
        {
            if *internal == type_name("Label") {
                constructions.push((ir.construction_targets[&expression], defaults.to_vec()));
            }
        }
        for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    constructions
}

const fn target(ordinal: u32) -> IrConstructorTarget {
    IrConstructorTarget {
        ordinal,
        access: IrConstructorAccess::Unrestricted,
    }
}

#[test]
fn a_construction_names_the_selected_primary_or_secondary_constructor() {
    let ir = lower_single_source(SOURCE, "ConstructionTargets");
    assert_eq!(
        label_constructions(&ir, "primaryWithDefault"),
        [(target(0), vec![1])]
    );
    assert_eq!(
        label_constructions(&ir, "primaryExplicit"),
        [(target(0), vec![])]
    );
    assert_eq!(
        label_constructions(&ir, "secondaryWithDefault"),
        [(target(1), vec![1])]
    );
    assert_eq!(
        label_constructions(&ir, "secondaryExplicit"),
        [(target(1), vec![])]
    );
    assert_eq!(
        label_constructions(&ir, "secondaryTagged"),
        [(target(2), vec![])]
    );
}

#[test]
fn a_secondary_ordinal_indexes_the_declared_secondary_constructor() {
    let ir = lower_single_source(SOURCE, "ConstructionTargets");
    let label = ir
        .classes
        .iter()
        .find(|class| class.fq_name_id() == type_name("Label"))
        .expect("class Label");
    let declared = label
        .secondary_ctors
        .iter()
        .map(|constructor| constructor.named_params.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        declared,
        [
            vec![
                ("base".to_owned(), Ty::obj("Base")),
                ("count".to_owned(), Ty::Int)
            ],
            vec![
                ("base".to_owned(), Ty::obj("Base")),
                ("tag".to_owned(), Ty::obj("Tag"))
            ],
        ]
    );
}

#[test]
fn a_constructor_reference_adapter_constructs_through_the_selected_constructor() {
    let ir = lower_single_source(SOURCE, "ConstructionTargets");
    let mut pending = vec![body(&ir, "taggedReference")];
    let mut adapters = Vec::new();
    while let Some(expression) = pending.pop() {
        if let IrExpr::CallableReference(reference) = ir.expr(expression) {
            adapters.push(reference.adapter);
        }
        for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    let [adapter] = adapters[..] else {
        panic!("one constructor reference, found {adapters:?}")
    };
    let adapter_body = ir.functions[adapter as usize]
        .body
        .expect("an adapter has a body");
    assert_eq!(
        constructions_under(&ir, adapter_body),
        [(target(2), vec![])]
    );
}

/// A construction of a class another file declares records the constructor declaration the
/// checker selected, and the constructing file and the declaring file carry one identical module
/// record for it: the qualified owner, the complete declared parameter list, and an inner class's
/// enclosing classifier.
#[test]
fn a_cross_file_construction_records_the_selected_constructor_and_its_declaration() {
    let sources = [
        (
            "package demo\nclass Pair(val a: Int, val b: Int) {\n    constructor(n: Int) : this(n, n)\n    inner class Part(val label: String)\n}\n",
            "defs",
        ),
        (
            "package demo\nfun primary(): Pair = Pair(1, 2)\nfun secondary(): Pair = Pair(3)\nfun part(): Pair.Part = Pair(4).Part(\"x\")\n",
            "box",
        ),
    ];
    let declaring = lower_source_from_set(&sources, 0);
    let constructing = lower_source_from_set(&sources, 1);
    let pair = type_name("demo/Pair");
    let part = type_name("demo/Pair$Part");
    let mut selected = Vec::new();
    for (id, expression) in constructing.exprs.iter().enumerate() {
        if let IrExpr::New { internal, .. } = expression {
            let constructor = constructing.module_constructions.selected[&(id as u32)];
            let record = &constructing.module_constructions.records[&constructor];
            assert_eq!(record.owner, *internal);
            assert_eq!(
                declaring.module_constructions.records.get(&constructor),
                Some(record),
                "the declaring file publishes the same record"
            );
            assert!(declaring
                .checked_constructor_bodies
                .contains_key(&constructor));
            selected.push((
                *internal,
                constructing.construction_targets[&(id as u32)].ordinal,
                record.parameters.to_vec(),
                record.outer,
            ));
        }
    }
    selected.sort_by_key(|(owner, ordinal, ..)| (*owner == part, *ordinal));
    assert_eq!(
        selected,
        [
            (pair, 0, vec![Ty::Int, Ty::Int], None),
            (pair, 1, vec![Ty::Int], None),
            (pair, 1, vec![Ty::Int], None),
            (part, 0, vec![Ty::String], Some(pair)),
        ]
    );
}
