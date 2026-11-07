use super::{enum_entry_bodies, names};
use crate::ir::{
    IrClass, IrClassDeclarationContext as Context, IrFile, IrLocalClassNameProvenance,
    IrLocalClassOwner, IrModuleSource,
};
use crate::metadata::type_encoder::StringTable;
use crate::types::{type_name, ClassifierDeclarationName, TypeName};

fn member(ir: &mut IrFile, owner: TypeName, name: &str) -> TypeName {
    let classifier = owner.nested_child(name);
    let mut class = IrClass::synthetic(classifier);
    class.declaration_context = Some(Context::Member);
    let owner_id = ir.class_id_by_name(owner).expect("declaring class");
    let owner_class = &ir.classes[owner_id as usize];
    if owner_class.is_local_class || owner_class.is_anonymous_object || owner_class.is_enum_entry {
        class.is_local_class = true;
        let class_id = ir.add_class(class);
        // Local/anonymous owners have no nonlocal declaration-name record in source headers.
        ir.local_class_name_provenance.insert(
            class_id,
            IrLocalClassNameProvenance {
                source: IrModuleSource {
                    source: crate::fir::SourceFileId::from_raw(0),
                    package: type_name("sample"),
                },
                lexical_owner: Some(IrLocalClassOwner::Class(owner_id)),
                segments: vec![name.to_owned()].into_boxed_slice(),
                ordinal: None,
                parents: Box::new([]),
            },
        );
        return classifier;
    }
    class.declaration_name = Some(ClassifierDeclarationName::Nested {
        owner,
        name: name.into(),
    });
    ir.add_class(class);
    classifier
}

fn entry(ir: &mut IrFile, name: &str) -> TypeName {
    let owner = type_name("sample/Choice");
    ir.add_class(IrClass::synthetic(owner));
    let classifier = member(ir, owner, name);
    let id = ir.class_id_by_name(classifier).expect("entry class");
    ir.classes[id as usize].is_enum_entry = true;
    classifier
}

fn initializer_object(ir: &mut IrFile, owner: TypeName, property: &str) -> TypeName {
    let classifier = owner.nested_child(property).nested_child("1");
    let mut class = IrClass::synthetic(classifier);
    class.is_anonymous_object = true;
    class.declaration_context = Some(Context::ClassInitializer { owner });
    ir.add_class(class);
    classifier
}

fn encoded_id(ir: &IrFile, classifier: TypeName) -> String {
    let known_inner = |_| Ok(crate::metadata::ClassifierApplication::Direct);
    let mut strings =
        StringTable::with_local_classifiers(&names(ir), &enum_entry_bodies(ir), None, &known_inner);
    let index = strings.class_id(classifier);
    strings.into_strings().remove(index as usize)
}

#[test]
fn initializer_objects_keep_the_entry_namespace_across_generated_segments() {
    let mut ir = IrFile::default();
    let entry = entry(&mut ir, "ENTRY");
    let object = initializer_object(&mut ir, entry, "storage");
    let inner = member(&mut ir, object, "Inner");
    assert_eq!(encoded_id(&ir, entry), "sample/Choice.ENTRY");
    assert_eq!(
        encoded_id(&ir, inner),
        "sample/Choice.ENTRY.storage.1.Inner"
    );
    let nested_object = initializer_object(&mut ir, inner, "nestedStorage");
    let deep = member(&mut ir, nested_object, "Deep");
    assert_eq!(
        encoded_id(&ir, deep),
        "sample/Choice.ENTRY.storage.1.Inner.nestedStorage.1.Deep"
    );
}

#[test]
fn a_literal_dollar_stays_part_of_its_declared_segment() {
    let mut ir = IrFile::default();
    let entry = entry(&mut ir, "ENTRY$part");
    let object = initializer_object(&mut ir, entry, "storage$value");
    let inner = member(&mut ir, object, "Inner$part");
    assert_eq!(
        encoded_id(&ir, inner),
        "sample/Choice.ENTRY$part.storage$value.1.Inner$part"
    );
}

#[test]
fn a_method_local_class_starts_a_raw_id_even_inside_an_entry() {
    let mut ir = IrFile::default();
    let entry = entry(&mut ir, "ENTRY");
    let local = entry.nested_child("make").nested_child("Local");
    let mut class = IrClass::synthetic(local);
    class.is_local_class = true;
    class.declaration_context = Some(Context::Executable);
    ir.add_class(class);
    let inner = member(&mut ir, local, "Inner");
    assert_eq!(
        encoded_id(&ir, inner),
        "sample/Choice$ENTRY$make$Local.Inner"
    );
    let object = initializer_object(&mut ir, local, "storage");
    let object_inner = member(&mut ir, object, "Nested");
    assert_eq!(
        encoded_id(&ir, object_inner),
        "sample/Choice$ENTRY$make$Local$storage$1.Nested"
    );
}

#[test]
fn an_ordinary_initializer_keeps_its_existing_raw_local_id() {
    let mut ir = IrFile::default();
    let owner = type_name("sample/Holder");
    ir.add_class(IrClass::synthetic(owner));
    let object = initializer_object(&mut ir, owner, "storage");
    let inner = member(&mut ir, object, "Inner");
    assert_eq!(encoded_id(&ir, inner), "sample/Holder$storage$1.Inner");
}
