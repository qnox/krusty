use super::*;

fn parse(source: &str) -> File {
    let mut diagnostics = crate::diag::DiagSink::new();
    let tokens = crate::lexer::lex(source, &mut diagnostics);
    let file = crate::parser::parse(source, &tokens, &mut diagnostics);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    file
}

fn named(file: &File, source: &str, name: &str) -> DeclId {
    let declarations = file
        .decls
        .iter()
        .copied()
        .filter(|declaration| match file.decl(*declaration) {
            Decl::Class(class) => {
                &source[class.name_span.lo as usize..class.name_span.hi as usize] == name
            }
            _ => false,
        })
        .collect::<Vec<_>>();
    assert_eq!(declarations.len(), 1, "one declaration of {name}");
    declarations[0]
}

#[test]
fn anonymous_members_keep_their_classifier_owner() {
    let source = include_str!("../../../tests/fixtures/captured_field_storage/anonymousLantern.kt");
    let file = parse(source);
    let lantern = named(&file, source, "Lantern");
    let flame = named(&file, source, "Flame");
    let anonymous = file
        .anonymous_object_classes
        .values()
        .copied()
        .collect::<Vec<_>>();
    let [wick] = anonymous.as_slice() else {
        panic!("one anonymous object expected: {anonymous:?}")
    };
    assert_eq!(file.hoisted_classifier_owner(flame), Some(*wick));
    assert_eq!(file.hoisted_classifier_owner(*wick), None);
    assert_eq!(file.hoisted_classifier_owner(lantern), None);
}

#[test]
fn body_members_keep_nearer_owners_and_do_not_claim_header_or_local_classes() {
    let source = "\
open class HeaderBase(val token: Any)
class Container {
    private val held = object : HeaderBase(object { inner class HeaderMember }) {
        inner class BodyMember { inner class Grandchild }
        fun local(): Any {
            class Local { inner class Leaf }
            return Local()
        }
    }
    class Sibling { class Child }
}
";
    let file = parse(source);
    let objects = file
        .anonymous_object_classes
        .values()
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(objects.len(), 2);
    let mut body = None;
    let mut header = None;
    for declaration in objects {
        let Decl::Class(class) = file.decl(declaration) else {
            panic!("anonymous declaration must be a classifier")
        };
        if class.base_class.is_some() {
            assert_eq!(body.replace(declaration), None);
        } else {
            assert_eq!(header.replace(declaration), None);
        }
    }
    let body = body.expect("anonymous subclass");
    let header = header.expect("anonymous header argument");
    let container = named(&file, source, "Container");
    let member = named(&file, source, "BodyMember");
    let local = named(&file, source, "Local");
    let sibling = named(&file, source, "Sibling");
    assert_eq!(file.hoisted_classifier_owner(member), Some(body));
    assert_eq!(
        file.hoisted_classifier_owner(named(&file, source, "Grandchild")),
        Some(member)
    );
    assert_eq!(
        file.hoisted_classifier_owner(named(&file, source, "HeaderMember")),
        Some(header)
    );
    let leaf = named(&file, source, "Leaf");
    let local_statement = file
        .local_class_decls
        .iter()
        .find_map(|(statement, declaration)| (*declaration == local).then_some(statement))
        .expect("the local classifier's declaring statement");
    assert_eq!(file.local_class_nested[local_statement], vec![leaf]);
    assert_eq!(file.hoisted_classifier_owner(leaf), None);
    assert_eq!(
        file.hoisted_classifier_owner(named(&file, source, "Child")),
        Some(sibling)
    );
    assert_eq!(file.hoisted_classifier_owner(sibling), Some(container));
    assert_eq!(file.hoisted_classifier_owner(local), None);
    assert_eq!(file.hoisted_classifier_owner(body), None);
    assert_eq!(file.hoisted_classifier_owner(header), None);
}
