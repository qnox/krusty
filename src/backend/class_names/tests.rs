use super::*;
use crate::ir::{IrClass, IrLocalClassNameProvenance};
use crate::types::type_name;

/// Declaration names published for a nested classifier and its owner, nothing else.
struct Published;

impl BackendClassifierSource for Published {
    fn classifier(
        &self,
        _: TypeName,
    ) -> Option<std::sync::Arc<super::super::BackendClassifierFact>> {
        None
    }

    fn classifier_declaration_name(
        &self,
        declaration: ClassifierDeclarationKey,
    ) -> Option<Option<ClassifierDeclarationName>> {
        let ClassifierDeclarationKey::Ordinary(identity) = declaration else {
            return None;
        };
        if identity == type_name("demo/sub/Outer") {
            Some(Some(ClassifierDeclarationName::TopLevel {
                package: type_name("demo/sub"),
                name: "Outer".into(),
            }))
        } else if identity == type_name("demo/sub/Outer$In$ner") {
            // A backticked segment containing `$` is one declared segment, not more nesting.
            Some(Some(ClassifierDeclarationName::Nested {
                owner: type_name("demo/sub/Outer"),
                name: "In$ner".into(),
            }))
        } else if identity == type_name("Root") {
            Some(Some(ClassifierDeclarationName::TopLevel {
                package: TypeName::ROOT,
                name: "Root".into(),
            }))
        } else {
            Some(None)
        }
    }
}

#[test]
fn published_names_follow_declaration_names_not_physical_spelling() {
    assert_eq!(
        published_classifier(&Published, type_name("demo/sub/Outer$In$ner")),
        Some(ReflectionNames {
            package: "demo.sub".into(),
            simple: Some("In$ner".into()),
            qualified: Some("demo.sub.Outer.In$ner".into()),
        })
    );
    assert_eq!(
        published_classifier(&Published, type_name("Root")),
        Some(ReflectionNames {
            package: String::new(),
            simple: Some("Root".into()),
            qualified: Some("Root".into()),
        })
    );
    // No published declaration name is no name, never one derived from the identity's spelling.
    assert_eq!(
        published_classifier(&Published, type_name("demo/sub/Unpublished")),
        None
    );
}

#[test]
fn declared_names_distinguish_member_local_and_anonymous_classes() {
    let mut ir = IrFile::default();
    let mut member = IrClass::synthetic(type_name("demo/Outer"));
    member.is_source_declared = true;
    let member = ir.add_class(member);
    let mut local = IrClass::synthetic(type_name("demo/Local"));
    local.is_source_declared = true;
    local.is_local_class = true;
    let local = ir.add_class(local);
    let mut anonymous = IrClass::synthetic(type_name("demo/Anonymous"));
    anonymous.is_source_declared = true;
    anonymous.is_anonymous_object = true;
    let anonymous = ir.add_class(anonymous);

    let names = declared_class(&ir, member);
    assert_eq!(names.kind(), ReflectionNameKind::Member);
    assert_eq!(
        names,
        ReflectionNames {
            package: "demo".into(),
            simple: Some("Outer".into()),
            qualified: Some("demo.Outer".into()),
        }
    );
    let names = declared_class(&ir, local);
    assert_eq!(names.kind(), ReflectionNameKind::Local);
    assert_eq!(
        names,
        ReflectionNames {
            package: "demo".into(),
            simple: Some("Local".into()),
            qualified: None,
        }
    );
    let names = declared_class(&ir, anonymous);
    assert_eq!(names.kind(), ReflectionNameKind::Anonymous);
    assert_eq!(names.package, "demo");
}

fn qualified(ir: &IrFile, class: ClassId) -> Option<String> {
    source_class_names(ir, class).map(|names| names.qualified)
}

fn declared(ir: &mut IrFile, name: TypeName) -> ClassId {
    let mut class = IrClass::synthetic(name);
    class.is_source_declared = true;
    ir.add_class(class)
}

/// A classifier declared in executable code under `owner` (the file when `None`), with its
/// source `segments` and, for an unnamed one, its ordinal.
fn local(
    ir: &mut IrFile,
    physical: &str,
    owner: Option<IrLocalClassOwner>,
    segments: &[&str],
    ordinal: Option<u32>,
) -> ClassId {
    let class = declared(ir, type_name(physical));
    ir.classes[class as usize].is_local_class = ordinal.is_none();
    ir.classes[class as usize].is_anonymous_object = ordinal.is_some();
    ir.local_class_name_provenance.insert(
        class,
        IrLocalClassNameProvenance {
            source: crate::ir::IrModuleSource {
                source: crate::fir::SourceFileId::from_raw(0),
                package: type_name("p/q"),
            },
            lexical_owner: owner,
            segments: segments.iter().map(|segment| segment.to_string()).collect(),
            ordinal,
            parents: Box::new([]),
        },
    );
    class
}

#[test]
fn a_local_class_is_named_by_its_package_and_enclosing_classifiers_only() {
    let mut ir = IrFile::default();
    let outer = declared(&mut ir, type_name("p/q/Outer"));
    let in_function = local(&mut ir, "p/q/L0", None, &["box", "Local"], None);
    let in_member = local(
        &mut ir,
        "p/q/L1",
        Some(IrLocalClassOwner::Class(outer)),
        &["make", "Made"],
        None,
    );
    let nested = local(
        &mut ir,
        "p/q/L2",
        Some(IrLocalClassOwner::Class(in_function)),
        &["Deep"],
        None,
    );
    assert_eq!(qualified(&ir, outer).as_deref(), Some("p.q.Outer"));
    assert_eq!(qualified(&ir, in_function).as_deref(), Some("p.q.Local"));
    assert_eq!(qualified(&ir, in_member).as_deref(), Some("p.q.Outer.Made"));
    assert_eq!(qualified(&ir, nested).as_deref(), Some("p.q.Local.Deep"));
}

#[test]
fn an_anonymous_object_has_no_name_provided() {
    let mut ir = IrFile::default();
    let outer = declared(&mut ir, type_name("p/q/Outer"));
    let in_file = local(&mut ir, "p/q/A0", None, &["box"], Some(1));
    let in_member = local(
        &mut ir,
        "p/q/A1",
        Some(IrLocalClassOwner::Class(outer)),
        &["make"],
        Some(1),
    );
    let external = local(
        &mut ir,
        "p/q/A2",
        Some(IrLocalClassOwner::External(
            type_name("p/r/Other").nested_child("Inner"),
        )),
        &["make"],
        Some(2),
    );
    assert_eq!(
        qualified(&ir, in_file).as_deref(),
        Some("p.q.<no name provided>")
    );
    assert_eq!(
        qualified(&ir, in_member).as_deref(),
        Some("p.q.Outer.<no name provided>")
    );
    assert_eq!(
        qualified(&ir, external).as_deref(),
        Some("p.r.Other.Inner.<no name provided>")
    );
}

#[test]
fn a_generated_classifier_has_no_source_name() {
    let mut ir = IrFile::default();
    let generated = ir.add_class(IrClass::synthetic(type_name("p/q/Generated")));
    let named_generated = local(&mut ir, "p/q/G1", None, &["box"], Some(3));
    ir.classes[named_generated as usize].is_anonymous_object = false;
    assert!(source_class_names(&ir, generated).is_none());
    assert!(source_class_names(&ir, named_generated).is_none());
}

#[test]
fn a_top_level_class_is_its_package_and_its_name() {
    let names = member_class_names(type_name("kotlin/IllegalStateException"));
    assert_eq!(names.qualified, "kotlin.IllegalStateException");
    assert_eq!(names.simple, "IllegalStateException");
}

#[test]
fn a_class_in_the_root_package_has_no_package_prefix() {
    let names = member_class_names(type_name("Foo"));
    assert_eq!(names.qualified, "Foo");
    assert_eq!(names.simple, "Foo");
}

#[test]
fn a_nested_class_is_joined_to_its_owner_with_a_dot() {
    let outer = type_name("p/q/Outer");
    let inner = outer.nested_child("Inner");
    let names = member_class_names(inner);
    assert_eq!(names.qualified, "p.q.Outer.Inner");
    assert_eq!(names.simple, "Inner");
}
