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
//! Interning orders are the serializer's and are read off fragments `kotlinc-native` writes: a
//! member's file name interns after its shared fields and before its constant, and the package's
//! qualified name interns after every member.

use super::builder::{
    write_package_members, ContractTypeTable, FnMeta, MemberTables, PackageMember, PackageMembers,
    PropMeta, TypeAliasMeta,
};
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

/// The `PackageFragment` bytes for one file declaring into `package` (its segments; empty for the
/// root package).
pub fn package_fragment(
    package: &[&str],
    file: &KlibFileMembers,
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

    let mut fragment = Pb::new();
    fragment.field_message(1, &st.string_table()); // PackageFragment.strings = 1
    fragment.field_message(2, &st.qualified_name_table()); // PackageFragment.qualified_names = 2
    fragment.field_message(3, &package_message); // PackageFragment.package = 3
    fragment.field_varint(172, 0); // isEmpty = 172
    fragment.field_bytes(173, package.join(".").as_bytes()); // fqName = 173
    fragment.canonical().into_bytes()
}
