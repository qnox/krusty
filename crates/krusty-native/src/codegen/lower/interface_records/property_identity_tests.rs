use krusty::backend::interface_identity::{member_identity, property_member_identity};
use krusty::libraries::{KlibDeclarationSignature, KlibDependencyCallable};
use krusty::metadata::id_signature::{
    accessor_signature, Accessor, ClassScope, DeclarationContainer, Placement, PropertyShape,
};
use krusty::metadata::semantic::{KotlinFunctionTypeShape, KotlinType};

#[test]
fn property_imports_join_either_accessor_without_aliasing_functions_or_roles() {
    let package = ["fixture".to_owned()];
    let classes = [ClassScope {
        name: "Cell",
        type_parameters: Vec::new(),
        expect: false,
    }];
    let container = DeclarationContainer {
        package: &package,
        classes: &classes,
        native_interop_library: false,
    };
    let property = PropertyShape {
        name: "count",
        contexts: Vec::new(),
        receiver: None,
        type_parameters: Vec::new(),
        expect: false,
        placement: Placement::Ordinary,
    };
    let value = KotlinType::Class {
        internal: "kotlin/Int".to_owned(),
        args: Vec::new(),
        nullable: false,
        shape: KotlinFunctionTypeShape::default(),
    };
    let getter = KlibDependencyCallable {
        module: "library".to_owned(),
        declaration: KlibDeclarationSignature::Accessor(
            accessor_signature(container, &property, Accessor::Getter).unwrap(),
        ),
    };
    let setter = KlibDependencyCallable {
        module: getter.module.clone(),
        declaration: KlibDeclarationSignature::Accessor(
            accessor_signature(container, &property, Accessor::Setter { value: &value }).unwrap(),
        ),
    };
    let read = property_member_identity(&getter, false).unwrap();
    let write = property_member_identity(&setter, true).unwrap();
    assert_eq!(read, property_member_identity(&setter, false).unwrap());
    assert_eq!(write, property_member_identity(&getter, true).unwrap());
    assert_ne!(read, write);
    assert_ne!(read, member_identity(&getter));
    assert_ne!(write, member_identity(&setter));
    let role_offset = 4 + getter.module.len();
    assert_eq!(read[role_offset], 2);
    assert_eq!(member_identity(&getter)[role_offset], 1);
    let another_module = KlibDependencyCallable {
        module: "unrelated".to_owned(),
        ..getter
    };
    assert_ne!(
        read,
        property_member_identity(&another_module, false).unwrap()
    );
}
