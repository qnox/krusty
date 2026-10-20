//! Exact ownership regressions for stable and deferred declaration type occurrences.

use crate::diag::DiagSink;
use crate::features::LangFeatures;
use crate::libraries::EmptySymbolSource;
use crate::source::SourceInput;
use crate::types::{Ty, TyKind};

fn local_class<'a>(
    file: &'a crate::ast::File,
    name: &str,
) -> (crate::ast::DeclId, &'a crate::ast::ClassDecl) {
    file.local_class_enclosing_declarations
        .keys()
        .copied()
        .find_map(|declaration| match file.decl(declaration) {
            crate::ast::Decl::Class(class) if class.name.rsplit('.').next() == Some(name) => {
                Some((declaration, class))
            }
            crate::ast::Decl::Fun(_)
            | crate::ast::Decl::Class(_)
            | crate::ast::Decl::Property(_) => None,
        })
        .unwrap_or_else(|| panic!("missing parser-hoisted local class {name}"))
}

#[test]
fn demanded_classifier_and_deferred_primary_constructor_keep_separate_markers() {
    let source = r#"
typealias Text = String

fun make() = {
    class Local(vararg val values: Text)
    "OK"
}()
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("MixedLocalHeaderOwnership")];
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);

    let source = crate::fir::SourceFileId::from_raw(0);
    let (parser_local, _) = local_class(&analysis.files[0], "Local");
    let streamed = analysis
        .streamed
        .take()
        .expect("the demanded classifier header must finalize in Pass 1");
    assert!(
        !streamed.diagnostic_recovery,
        "the ownership assertions require the ordinary Pass-1 index"
    );
    let (mut index, _, _, _) = streamed.module.into_parts();
    let active = crate::fir::ActiveSourceDeclarations::bind_complete_source(
        &analysis.files[0],
        source,
        &index,
    )
    .expect("local declarations bind to their stable inventory");
    let classifier = active
        .canonical_classifier_declaration(parser_local, &index)
        .expect("stable local classifier declaration");
    let constructor = index
        .owned_declaration(classifier, crate::fir::DeclarationKind::Constructor, 0)
        .expect("stable primary constructor declaration");
    let property = active
        .constructor_property_declaration(parser_local, 0)
        .expect("stable constructor-property declaration");

    assert!(index.classifier_header(classifier).is_some());
    assert!(index
        .declaration_spellings(classifier)
        .expect("the demanded classifier owns an explicit Pass-1 marker")
        .params
        .is_empty());
    assert!(index.declaration_spellings(constructor).is_none());
    assert!(index.signature(constructor).is_none());
    let property_result_before = index
        .signature(property)
        .expect("the independently demanded constructor property finalizes in Pass 1")
        .result
        .get();
    let property_spelling_before = index
        .declaration_spellings(property)
        .expect("the demanded constructor property owns its Pass-1 spelling")
        .ret
        .clone();
    assert_eq!(
        property_spelling_before.alias,
        Some(crate::types::type_name("Text"))
    );

    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        source,
        &analysis.symbols,
        analysis.types[0]
            .as_ref()
            .expect("Pass 2 checked local declarations"),
        &mut index,
    )
    .expect("deferred primary constructor publication");
    assert_eq!(
        index
            .declaration_spellings(classifier)
            .expect("classifier spelling completion")
            .params
            .len(),
        1
    );
    assert_eq!(
        index
            .declaration_spellings(constructor)
            .expect("constructor spelling publication")
            .params
            .len(),
        1
    );
    let constructor_signature = index
        .signature(constructor)
        .expect("constructor signature publication");
    assert_eq!(constructor_signature.parameters.len(), 1);
    assert_eq!(
        property_result_before,
        constructor_signature.parameters[0].get(),
        "the stable property role and deferred constructor slot must join on one semantic type"
    );
    assert_eq!(
        &property_spelling_before,
        index
            .declaration_spellings(constructor)
            .expect("constructor spelling publication")
            .param(0),
        "the stable property role and deferred constructor slot must retain one source spelling"
    );
}

#[test]
fn demanded_local_constructor_retains_an_explicit_empty_spelling_marker() {
    let source = r#"
fun make() = {
    class Local(val value: String)
    Local("OK")
}()
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("EmptyLocalConstructorSpelling")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);

    let source = crate::fir::SourceFileId::from_raw(0);
    let (parser_local, _) = local_class(&analysis.files[0], "Local");
    let index = analysis
        .streamed
        .as_ref()
        .expect("the explicit primary type must demand the constructor header")
        .module
        .index();
    let active = crate::fir::ActiveSourceDeclarations::bind_complete_source(
        &analysis.files[0],
        source,
        index,
    )
    .expect("local declarations bind to their stable inventory");
    let classifier = active
        .canonical_classifier_declaration(parser_local, index)
        .expect("stable local classifier declaration");
    let constructor = index
        .owned_declaration(classifier, crate::fir::DeclarationKind::Constructor, 0)
        .expect("stable primary constructor declaration");

    assert_eq!(
        index
            .signature(constructor)
            .expect("the demanded constructor signature is finalized")
            .parameters
            .len(),
        1
    );
    assert!(
        index
            .declaration_spellings(constructor)
            .expect("the demanded constructor owns an explicit spelling marker")
            .is_none(),
        "a plain String type has no metadata spelling payload"
    );
}

#[test]
fn deferred_cross_formal_bound_corrects_root_and_alias_argument_occurrences() {
    let source = r#"
open class Floor
interface Order<X>
typealias Alias<X> = Order<X>
fun make(): Any {
    class Local {
        fun <T, U> first(vararg values: T): T where T : Alias<U>, U : Floor = values[0]
    }
    return Local()
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("DeferredCrossFormalBounds")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);

    let (parser_local, local) = local_class(&analysis.files[0], "Local");
    let method_index = local
        .methods
        .iter()
        .position(|method| method.name == "first")
        .expect("local generic method");
    let method = &local.methods[method_index];
    let index = analysis
        .streamed
        .as_ref()
        .expect("the selected local method must finalize")
        .module
        .index();
    let active = crate::fir::ActiveSourceDeclarations::bind_complete_source(
        &analysis.files[0],
        crate::fir::SourceFileId::from_raw(0),
        index,
    )
    .expect("local declarations bind to their stable inventory");
    let stable_first = active
        .source_member_declaration(
            &analysis.files[0],
            index,
            crate::libraries::SourceMember::Class {
                file: 0,
                owner: parser_local.0,
                method: u32::try_from(method_index).expect("local method index fits u32"),
            },
        )
        .expect("stable local generic method");
    assert!(
        !index.declaration_semantic_header_complete(stable_first),
        "an undemanded local method's semantic header and source occurrences belong to active Pass 2"
    );
    assert!(
        index.declaration_spellings(stable_first).is_none(),
        "the undemanded local method has not published source occurrences separately"
    );

    let alias_bound = method
        .type_param_bounds
        .iter()
        .find_map(|(owner, bound)| (owner == "T").then_some(bound))
        .expect("T : Alias<U> bound");
    let info = analysis.types[0]
        .as_ref()
        .expect("Pass 2 checked local declarations");
    let (root, _) = info
        .resolved_type_bound(alias_bound)
        .expect("checked root bound occurrence");
    assert_eq!(root.obj_internal(), Some(crate::types::type_name("Order")));
    let [root_argument] = root.type_args() else {
        panic!("Alias<U> expands to one Order argument: {root:?}")
    };
    let TyKind::TyParam(root_formal, root_bound) = root_argument.kind() else {
        panic!("Order argument retains U's semantic identity: {root_argument:?}")
    };
    assert_eq!(root_bound, Ty::obj("Floor"));

    let spelling = info
        .resolved_declaration_spelling(alias_bound)
        .expect("checked alias-bound spelling");
    assert_eq!(spelling.alias, Some(crate::types::type_name("Alias")));
    let [(alias_argument, _)] = spelling.alias_args.as_slice() else {
        panic!("Alias<U> retains one abbreviated argument")
    };
    let TyKind::TyParam(alias_formal, alias_bound) = alias_argument.kind() else {
        panic!("abbreviated Alias argument retains U: {alias_argument:?}")
    };
    assert_eq!(alias_formal, root_formal);
    assert_eq!(alias_bound, Ty::obj("Floor"));
}

#[test]
fn property_and_getter_aliases_keep_distinct_spelling_roles() {
    let source = r#"
package sample

class Item
typealias PropertyAlias = Item
typealias GetterAlias = Item

fun build(): Item {
    class Local {
        val value: PropertyAlias
            get(): GetterAlias = Item()
    }
    return Local().value
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("DistinctGetterSpelling")];
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let (parser_local, local) = local_class(&analysis.files[0], "Local");
    let property_index = local
        .body_props
        .iter()
        .position(|property| property.name == "value")
        .expect("local value property");
    let streamed = analysis
        .streamed
        .take()
        .expect("the selected local property must finalize")
        .module;
    let (mut index, _, _, _) = streamed.into_parts();
    let active = crate::fir::ActiveSourceDeclarations::bind_complete_source(
        &analysis.files[0],
        crate::fir::SourceFileId::from_raw(0),
        &index,
    )
    .expect("local declarations bind to their stable inventory");
    let value = active
        .class_body_property_declaration(
            parser_local,
            u32::try_from(property_index).expect("local property index fits u32"),
        )
        .expect("stable local value declaration");
    assert!(
        !index.declaration_semantic_header_complete(value),
        "an undemanded local property's semantic header and source occurrences belong to active Pass 2"
    );
    assert!(
        index.declaration_spellings(value).is_none(),
        "the undemanded local property has not published source occurrences separately"
    );
    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        crate::fir::SourceFileId::from_raw(0),
        &analysis.symbols,
        analysis.types[0]
            .as_ref()
            .expect("Pass 2 checked local declarations"),
        &mut index,
    )
    .expect("local property spelling publication");
    let spellings = index
        .declaration_spellings(value)
        .expect("finalized local property spellings");
    assert_eq!(
        spellings.ret.alias,
        Some(crate::types::type_name("sample/PropertyAlias"))
    );
    assert_eq!(
        spellings.getter_ret.alias,
        Some(crate::types::type_name("sample/GetterAlias"))
    );
}
