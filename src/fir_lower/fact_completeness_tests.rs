//! Common lowering publishes a layout for every property a file declares and records every
//! parameter that carries a shared capture holder; `IrFile::validate_complete_facts` proves both
//! before a backend runs. These tests pin what the tables hold for the shapes a backend would
//! otherwise have looked for by spelling or by scanning a body.

use super::tests::lower_single_source;
use crate::ir::{IrFile, IrLocalPropertyLayout};
use crate::types::{type_name, Ty, TypeName};

/// Every property layout of the file as `(declared name, layout kind, qualifier or owner)`,
/// sorted.
fn layouts(ir: &IrFile) -> Vec<(String, &'static str, Option<TypeName>)> {
    let mut layouts = ir
        .local_property_layouts
        .iter()
        .map(|(property, layout)| {
            let name = ir.checked_properties[property].name.clone();
            let (kind, owner) = match layout {
                IrLocalPropertyLayout::TopLevelStorage { qualifier, .. } => {
                    ("top-level storage", *qualifier)
                }
                IrLocalPropertyLayout::TopLevelAccessor { .. } => ("top-level accessor", None),
                IrLocalPropertyLayout::Member { owner, .. } => ("member", Some(*owner)),
                IrLocalPropertyLayout::MemberExtension { owner, .. } => {
                    ("member extension", Some(*owner))
                }
            };
            (name, kind, owner)
        })
        .collect::<Vec<_>>();
    layouts.sort_by(|left, right| left.0.cmp(&right.0));
    layouts
}

#[test]
fn every_property_the_file_declares_has_its_layout() {
    let ir = lower_single_source(
        "val stored: Int = 1
val computed: Int
    get() = stored + 1
val String.twice: String
    get() = this + this
class Host(val given: Int) {
    var counted: Int = 0
    val Int.doubled: Int
        get() = this * 2
    companion object {
        const val LIMIT: Int = 3
    }
}
object Registry {
    const val NAME: String = \"r\"
}
",
        "Layouts",
    );
    ir.validate_complete_facts(crate::fir::SourceFileId::from_raw(0))
        .expect("complete facts");
    let owned = |owner: &str| Some(type_name(owner));
    assert_eq!(
        layouts(&ir),
        [
            (
                "LIMIT".to_owned(),
                "top-level storage",
                owned("Host$Companion")
            ),
            ("NAME".to_owned(), "top-level storage", owned("Registry")),
            ("computed".to_owned(), "top-level accessor", None),
            ("counted".to_owned(), "member", owned("Host")),
            ("doubled".to_owned(), "member extension", owned("Host")),
            ("given".to_owned(), "member", owned("Host")),
            ("stored".to_owned(), "top-level storage", None),
            ("twice".to_owned(), "top-level accessor", None),
        ]
    );
}

#[test]
fn a_lambda_that_only_passes_a_captured_holder_on_records_it() {
    let ir = lower_single_source(
        "fun outer(): Int {
    var count = 1
    val make = {
        object {
            fun read(): Int = count
        }
    }
    count = 2
    return make().read()
}
",
        "PassedHolder",
    );
    ir.validate_complete_facts(crate::fir::SourceFileId::from_raw(0))
        .expect("complete facts");
    // The lambda receives the holder only to hand it to the object's constructor; it never reads
    // it. Its parameter is recorded all the same, so no backend has to infer it from a body.
    let recorded = ir
        .shared_capture_parameters
        .iter()
        .map(|(key, ty)| (*key, *ty))
        .collect::<Vec<_>>();
    let [((lambda, parameter), ty)] = recorded.as_slice() else {
        panic!("the fixture has exactly one shared capture parameter: {recorded:?}");
    };
    let key = (*lambda, *parameter);
    assert_eq!((*parameter, *ty), (0, Ty::Int));
    // Without the record, the lambda's capture of the holder alone obliges it.
    let mut incomplete = ir;
    incomplete.shared_capture_parameters.remove(&key);
    assert_eq!(
        incomplete.validate_complete_facts(crate::fir::SourceFileId::from_raw(0)),
        Err(crate::ir::IncompleteIrFact::SharedCaptureParameter {
            function: key.0,
            parameter: key.1,
        })
    );
}

#[test]
fn a_local_function_that_only_forwards_a_captured_holder_records_it() {
    let ir = lower_single_source(
        "fun outer(): Int {
    var count = 1
    fun make() = object {
        fun read(): Int = count
    }
    count = 2
    return make().read()
}
",
        "LocalPassedHolder",
    );
    ir.validate_complete_facts(crate::fir::SourceFileId::from_raw(0))
        .expect("complete facts");
    let recorded = ir
        .shared_capture_parameters
        .iter()
        .map(|(key, ty)| (*key, *ty))
        .collect::<Vec<_>>();
    let [((function, parameter), ty)] = recorded.as_slice() else {
        panic!("the fixture has exactly one shared capture parameter: {recorded:?}");
    };
    let key = (*function, *parameter);
    assert_eq!((*parameter, *ty), (0, Ty::Int));
    let mut incomplete = ir;
    incomplete.shared_capture_parameters.remove(&key);
    assert_eq!(
        incomplete.validate_complete_facts(crate::fir::SourceFileId::from_raw(0)),
        Err(crate::ir::IncompleteIrFact::SharedCaptureParameter {
            function: key.0,
            parameter: key.1,
        })
    );
}
