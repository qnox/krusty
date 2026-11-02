use super::*;
use crate::libraries::{LibraryType, ResolvedSymbols};
use crate::symbol_source::SymbolNamespace;
use std::sync::Arc;

struct SelectedOwner {
    owner: TypeName,
    class: Arc<LibraryType>,
}

impl SymbolSource for SelectedOwner {
    fn symbols(&self, namespace: SymbolNamespace, name: &str) -> std::rc::Rc<ResolvedSymbols> {
        let selected = namespace.existing_classifier(name) == Some(self.owner);
        std::rc::Rc::new(ResolvedSymbols {
            classifier_name: selected.then_some(self.owner),
            classifier: selected.then(|| self.class.clone()),
            ..Default::default()
        })
    }
}

impl SemanticPlatform for SelectedOwner {}

#[test]
fn exact_provider_formals_preserve_own_and_captured_nullable_arguments() {
    let mut diagnostics = DiagSink::new();
    let tokens = crate::lexer::lex("", &mut diagnostics);
    let file = crate::parser::parse("", &tokens, &mut diagnostics);
    let owner = type_name("hierarchy/Selected");
    let mut class = LibraryType::declaration_header();
    class.own_type_parameter_count = 1;
    class.type_parameters = crate::types::TypeParameters::invariant(
        vec!["owner:Own".into(), "outer:Captured".into()],
        vec![
            vec![Ty::nullable(Ty::obj("kotlin/Any"))],
            vec![Ty::nullable(Ty::obj("kotlin/Any"))],
        ],
    );
    let mut symbols = SymbolTable::default();
    symbols.libraries = Box::new(SelectedOwner {
        owner,
        class: Arc::new(class),
    });
    let mut checker = make_checker(&file, 0, None, &symbols, &mut diagnostics);
    let own = Ty::ty_param("owner:Own", Ty::nullable(Ty::obj("kotlin/Any")));
    let captured = Ty::ty_param("outer:Captured", Ty::nullable(Ty::obj("kotlin/Any")));
    checker.record_checked_body_local_supertypes(
        Some(owner),
        &[Ty::nullable(Ty::obj_args(
            "hierarchy/Parent",
            &[Ty::nullable(own), captured],
        ))],
    );
    let receiver = Ty::nullable(Ty::obj_args_name(owner, &[Ty::String, Ty::Int]));
    assert_eq!(
        checker.body_local_supertypes(receiver),
        [Ty::nullable(Ty::obj_args(
            "hierarchy/Parent",
            &[Ty::nullable(Ty::String), Ty::Int]
        ))]
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn checked_edges_without_a_provider_never_reenter_inherited_name_recovery() {
    let mut diagnostics = DiagSink::new();
    let tokens = crate::lexer::lex("", &mut diagnostics);
    let file = crate::parser::parse("", &tokens, &mut diagnostics);
    let symbols = SymbolTable::default();
    let mut checker = make_checker(&file, 0, None, &symbols, &mut diagnostics);
    let owner = type_name("hierarchy/Unpublished");
    checker.lexical_class_context.push(owner);
    checker.exact_anonymous_class_roots.insert(owner);
    let unbound = Ty::ty_param("declaration:Unbound", Ty::nullable(Ty::obj("kotlin/Any")));
    let parent = Ty::obj_args("hierarchy/Parent", &[Ty::nullable(unbound)]);
    checker.record_checked_body_local_supertypes(Some(owner), &[parent]);
    assert_eq!(checker.body_local_supertypes(Ty::obj_name(owner)), [parent]);
    assert!(checker.fed_source().classifier(owner).is_none());
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn deferred_owner_uses_its_active_declaration_formals_without_a_provider() {
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_prefix_with_features(
        &[crate::source::SourceInput::kotlin(
            "package hierarchy
interface Parent<T>
fun scope() { typealias Alias<T> = Parent<T?>; class Local<Own> : Alias<Own> }",
        )],
        0,
        1,
        crate::frontend::PlatformProvider::jvm(Box::new(crate::libraries::EmptySymbolSource)),
        &crate::features::LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let file = &analysis.files[0];
    let (parser, class) = file
        .local_class_decls
        .values()
        .find_map(|&declaration| match file.decl(declaration) {
            Decl::Class(class) => Some((declaration, class)),
            _ => None,
        })
        .expect("one source-owned local classifier");
    let streamed = analysis
        .streamed
        .take()
        .expect("finalized module inventory");
    let index = streamed.module.index();
    let active = crate::fir::ActiveSourceDeclarations::bind_complete_source(
        file,
        crate::fir::SourceFileId::from_raw(0),
        index,
    )
    .expect("exact parser-to-declaration binding");
    let declaration = active
        .canonical_classifier_declaration(parser, index)
        .unwrap();
    let owner = index.classifier_identity(declaration).unwrap();
    assert!(index.classifier_header(declaration).is_none());
    let mut checker = make_checker_with_index(
        file,
        0,
        None,
        &analysis.symbols,
        Some(index),
        Some(&active),
        None,
        &mut diagnostics,
    );
    checker
        .resolved_declaration_type_parameters
        .insert(class.span.lo, vec!["checked:Own".into()]);
    let formal = Ty::ty_param("checked:Own", Ty::nullable(Ty::obj("kotlin/Any")));
    checker.record_checked_body_local_supertypes(
        Some(owner),
        &[Ty::obj_args("hierarchy/Parent", &[Ty::nullable(formal)])],
    );
    assert!(checker.fed_source().classifier(owner).is_none());
    assert_eq!(
        checker.body_local_supertypes(Ty::nullable(Ty::obj_args_name(owner, &[Ty::String]))),
        [Ty::obj_args(
            "hierarchy/Parent",
            &[Ty::nullable(Ty::String)]
        )]
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn a_representable_local_header_is_published_before_ordinary_body_checking() {
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_prefix_with_features(
        &[crate::source::SourceInput::kotlin(
            "fun scope() { class Local<Own> }",
        )],
        0,
        1,
        crate::frontend::PlatformProvider::jvm(Box::new(crate::libraries::EmptySymbolSource)),
        &crate::features::LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let file = &analysis.files[0];
    let parser = *file
        .local_class_decls
        .values()
        .next()
        .expect("one local classifier");
    let index = analysis
        .streamed
        .as_ref()
        .expect("finalized module inventory")
        .module
        .index();
    let active = crate::fir::ActiveSourceDeclarations::bind_complete_source(
        file,
        crate::fir::SourceFileId::from_raw(0),
        index,
    )
    .expect("exact parser-to-declaration binding");
    let declaration = active
        .canonical_classifier_declaration(parser, index)
        .unwrap();
    assert_eq!(
        index
            .classifier_header(declaration)
            .map(|header| header.classifier),
        index.classifier_identity(declaration),
    );
    assert!(index.classifier_header(declaration).is_some());
}
