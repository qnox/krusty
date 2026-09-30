use crate::diag::DiagSink;
use crate::features::LangFeatures;
use crate::fir::DeclarationFlags;
use crate::libraries::EmptySymbolSource;
use crate::source::SourceInput;
use crate::types::Ty;

#[test]
fn constructor_and_body_properties_share_a_sibling_ordinal_and_not_the_parameter_flag() {
    let source = "class Version(private vararg val numbers: Int) {\n    val major: Int = 1\n}\n";
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("VarargPropertyFlag")],
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
    let property = |name: &str| {
        (0..index.declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .find(|declaration| index.declaration_name(*declaration) == Some(name))
            .unwrap_or_else(|| panic!("property {name}"))
    };
    let numbers = property("numbers");
    let major = property("major");
    let numbers_anchor = index.declaration_anchor(numbers).expect("numbers anchor");
    let major_anchor = index.declaration_anchor(major).expect("major anchor");
    assert_eq!(numbers_anchor.sibling, major_anchor.sibling);
    assert_eq!(numbers_anchor.owner, major_anchor.owner);
    assert!(index
        .declaration_header(numbers)
        .expect("numbers header")
        .flags
        .has(DeclarationFlags::PROPERTY_PARAMETER));
    assert!(!index
        .declaration_header(major)
        .expect("major header")
        .flags
        .has(DeclarationFlags::PROPERTY_PARAMETER));
    assert_eq!(
        index
            .signature(numbers)
            .expect("numbers signature")
            .result
            .get(),
        Ty::array(Ty::Int)
    );
    assert_eq!(
        index
            .signature(major)
            .expect("major signature")
            .result
            .get(),
        Ty::Int
    );
}
