//! The KLIB carrier of Kotlin metadata: one `PackageFragment` per source file.
//!
//! A KLIB stores the same declaration schema the JVM `@Metadata` annotation does, so the records come
//! from the shared builders ([`super::builder`]). What differs is the carrier, and this module owns
//! exactly that difference:
//!
//! - the fragment carries its own `StringTable` (f1) and `QualifiedNameTable` (f2) instead of `d2`
//!   strings and `StringTableTypes` records;
//! - a container's types live in its `TypeTable` (`Package.type_table` = 30), referenced by id;
//! - each declaration names the file declaring it (`functionFile` = 172, `propertyFile` = 176), and
//!   a `const val` carries its value (`compileTimeValue` = 173), which a JVM class file keeps in a
//!   `ConstantValue` attribute instead;
//! - the fragment names its package twice: `Package.packageFqName` (171) indexes the qualified-name
//!   table, `PackageFragment.fqName` (173) spells it.
//!
//! - the fragment carries the file's classes, nested ones included, each after the classes it is
//!   nested in, and lists their qualified names (`className` = 174). A class keeps its own
//!   `TypeTable` and names its file (`classFile` = 175).
//!
//! Interning orders are the serializer's and are read off fragments `kotlinc-native` writes: the
//! package's members come first, then the package's qualified name, then the classes in order; a
//! member's file name interns after its shared fields and before its constant.

use super::builder::{
    write_package_members, ContractTypeTable, FnMeta, MemberTables, PackageMember, PackageMembers,
    PropMeta, TypeAliasMeta,
};
use super::class_builder::{class_message, ClassDeclaration, KlibCarrier};
use super::protobuf::Pb;
use super::type_encoder::StringTable;
use super::version_requirements::VersionRequirementTable;

/// The top-level declarations of one source file.
pub struct KlibFileMembers {
    /// The file's name as the fragment records it (`lib.kt`), without its directory.
    pub file_name: String,
    pub functions: Vec<FnMeta>,
    pub properties: Vec<PropMeta>,
    /// Parallel to `properties`: a `const val`'s value, which a KLIB records with the property.
    pub constants: Vec<Option<crate::ir::IrConst>>,
    pub aliases: Vec<TypeAliasMeta>,
}

/// One class of the file, as the fragment records it.
pub(crate) struct KlibClass<'a> {
    pub(crate) declaration: ClassDeclaration<'a>,
    /// Parallel to the declaration's properties: a `const val`'s value.
    pub(crate) constants: &'a [Option<crate::ir::IrConst>],
}

/// The `PackageFragment` bytes for one file declaring into `package` (its segments; empty for the
/// root package): its top-level members and `classes`, in fragment order.
pub(crate) fn package_fragment(
    package: &[&str],
    file: &KlibFileMembers,
    classes: &[KlibClass<'_>],
    annotations_in_metadata: bool,
) -> Vec<u8> {
    let mut st = StringTable::klib();
    let mut requirements = VersionRequirementTable::default();
    let mut contract_types = ContractTypeTable::default();
    let mut package_message = Pb::new();
    st.open_type_table();
    write_package_members(
        &mut package_message,
        &mut st,
        PackageMembers {
            functions: &file.functions,
            properties: &file.properties,
            aliases: &file.aliases,
        },
        &mut MemberTables {
            requirements: &mut requirements,
            contract_types: &mut contract_types,
        },
        // A KLIB records no JVM parameter-assertion requirement.
        (false, annotations_in_metadata),
        &mut |st, member, record| match member {
            PackageMember::Function(_) => {
                let file_name = st.local(&file.file_name);
                record.field_varint(172, u64::from(file_name)); // functionFile = 172
            }
            PackageMember::Property(index) => {
                let file_name = st.local(&file.file_name);
                record.field_varint(176, u64::from(file_name)); // propertyFile = 176
                if let Some(Some(constant)) = file.constants.get(index) {
                    let value = super::builder::constant_value_pb(st, constant);
                    record.field_message(173, &value); // compileTimeValue = 173
                }
            }
            PackageMember::Alias(_) => {}
        },
    );
    if let Some(table) = st.close_type_table() {
        package_message.field_message(30, &table); // Package.type_table = 30
    }
    if let Some(table) = requirements.encode() {
        package_message.field_message(32, &table); // Package.version_requirement_table = 32
    }
    // packageFqName = 171; the root package, which has no qualified name, is written as -1.
    let package_name = st.klib_package(package).map_or(-1_i64, i64::from);
    package_message.field_varint(171, package_name as u64);
    let mut class_messages = Vec::new();
    let mut class_names = Vec::new();
    for class in classes {
        let carrier = KlibCarrier {
            source_file: &file.file_name,
            constants: class.constants,
        };
        class_messages.push(class_message(&mut st, &class.declaration, &carrier));
        class_names.push(u64::from(st.class_id(class.declaration.name)));
    }

    let mut fragment = Pb::new();
    fragment.field_message(1, &st.string_table()); // PackageFragment.strings = 1
    fragment.field_message(2, &st.qualified_name_table()); // PackageFragment.qualified_names = 2
    fragment.field_message(3, &package_message); // PackageFragment.package = 3
    for class in &class_messages {
        fragment.repeated_message(4, class); // PackageFragment.class = 4
    }
    fragment.field_varint(172, 0); // isEmpty = 172
    fragment.field_bytes(173, package.join(".").as_bytes()); // fqName = 173
    fragment.field_packed_varints(174, &class_names); // className = 174, packed
    fragment.canonical().into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::semantic::{parse_package_fragment_checked, KotlinType};
    use crate::types::Ty;

    fn constant_property(name: &str) -> PropMeta {
        PropMeta {
            spellings: crate::spelling::DeclaredSpellings::default(),
            visibility: crate::types::Visibility::Public,
            name: name.into(),
            ty: Ty::Int,
            is_var: false,
            type_params: Vec::new(),
            semantic_type_params: Vec::new(),
            type_param_bounds: Vec::new(),
            receiver: None,
            context_params: Vec::new(),
            getter: None,
            setter: None,
            setter_parameter_name: None,
            is_const: true,
            has_constant: true,
            has_backing_field: true,
            modifiers: crate::ir::IrPropertyModifiers::default(),
            setter_visibility: crate::types::Visibility::Public,
            companion: false,
            accessor_annotations: Default::default(),
            field_name: None,
            field_desc: None,
            decl_order: 1,
        }
    }

    fn file_members() -> KlibFileMembers {
        KlibFileMembers {
            file_name: "lib.kt".to_string(),
            functions: vec![FnMeta::plain(
                "answer",
                vec![("x".into(), Ty::Int)],
                Ty::String,
            )],
            properties: vec![constant_property("LIMIT")],
            constants: vec![Some(crate::ir::IrConst::Int(3))],
            aliases: Vec::new(),
        }
    }

    /// The fragment's trailing fields: `isEmpty` (172) is 0 and `fqName` (173) spells the package.
    fn fragment_tail(package: &str) -> Vec<u8> {
        let mut tail = Pb::new();
        tail.field_varint(172, 0);
        tail.field_bytes(173, package.as_bytes());
        tail.into_bytes()
    }

    #[test]
    fn a_fragment_names_its_package_and_reads_back_through_the_klib_reader() {
        let fragment = package_fragment(&["p", "q"], &file_members(), &[], true);
        assert!(fragment.ends_with(&fragment_tail("p.q")));

        let package = parse_package_fragment_checked(&fragment).expect("a decodable fragment");
        assert_eq!(package.classes.len(), 0);
        assert_eq!(package.functions.len(), 1);
        let function = &package.functions[0];
        assert_eq!(function.name, "answer");
        assert_eq!(function.param_names, ["x"]);
        assert_eq!(function.params, [KotlinType::class("kotlin/Int")]);
        assert_eq!(function.ret, KotlinType::class("kotlin/String"));
        assert_eq!(package.properties.len(), 1);
        let property = &package.properties[0];
        assert_eq!(property.name, "LIMIT");
        assert_eq!(property.constant, Some(crate::libraries::LibConst::Int(3)));
    }

    #[test]
    fn a_root_package_fragment_has_an_empty_name() {
        let fragment = package_fragment(&[], &file_members(), &[], true);
        assert!(fragment.ends_with(&fragment_tail("")));
        let package = parse_package_fragment_checked(&fragment).expect("a decodable fragment");
        assert_eq!(package.functions.len(), 1);
        assert_eq!(package.properties.len(), 1);
    }

    #[test]
    fn a_class_is_written_after_the_package_and_reads_back_through_the_klib_reader() {
        use crate::metadata::class_builder::{ClassDeclaration, ClassTail};
        let methods = [crate::metadata::class_builder::FnMeta::plain(
            "sum".to_string(),
            Vec::new(),
            Ty::Int,
        )];
        let tail = ClassTail::default();
        let point = ClassDeclaration {
            name: crate::types::type_name("p/q/Point"),
            ctor_params: &[("x".to_string(), Ty::Int)],
            props: &[],
            methods: &methods,
            enum_entries: &[],
            tail: &tail,
        };
        let point = KlibClass {
            declaration: point,
            constants: &[],
        };
        let fragment = package_fragment(&["p", "q"], &file_members(), &[point], true);
        let package = parse_package_fragment_checked(&fragment).expect("a decodable fragment");
        assert_eq!(package.functions.len(), 1);
        assert_eq!(
            package
                .classes
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["p/q/Point"]
        );
        let class = &package.classes["p/q/Point"];
        assert_eq!(class.constructors.len(), 1);
        assert_eq!(
            class.constructors[0].params,
            [KotlinType::class("kotlin/Int")]
        );
        assert_eq!(
            class
                .members
                .iter()
                .map(|member| (member.name.as_str(), member.is_property, member.ret.clone()))
                .collect::<Vec<_>>(),
            [("sum", false, KotlinType::class("kotlin/Int"))]
        );
    }
}
