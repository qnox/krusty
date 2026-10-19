use super::*;

#[test]
fn application_facts_project_to_each_site_without_a_canonical_header() {
    let facts = SelectedTypeFacts::default();
    let application = SigExprId::from_raw(7);
    let first = HeaderTypeId::from_raw(3);
    let second = HeaderTypeId::from_raw(9);
    let selected = super::super::super::type_spellings::ResolvedTypeSpelling {
        classifier_segment: Some(2),
        alias: Some(crate::types::type_name("sample/Selected")),
        resolved: None,
    };
    facts.record_application(application, selected);
    facts.record_spelling(
        first,
        super::super::super::type_spellings::ResolvedTypeSpelling {
            alias: Some(crate::types::type_name("sample/FirstHeader")),
            ..Default::default()
        },
    );

    assert!(facts.project_application_to_declaration_sites(
        application,
        crate::types::Ty::String,
        &[first, second],
    ));
    assert_eq!(facts.spelling(first), facts.spelling(second));
    assert_eq!(
        facts.spelling(first),
        Some(super::super::super::type_spellings::ResolvedTypeSpelling {
            resolved: Some(crate::types::Ty::String),
            ..selected
        })
    );
}
