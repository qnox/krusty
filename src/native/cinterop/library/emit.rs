//! An interop library's declarations as KLIB `PackageFragment`s.
//!
//! The messages are Kotlin metadata's, written through the shared protobuf writer and string,
//! qualified-name and type tables ([`StringTable`]). What is cinterop's own is the split into
//! fragments, which is kotlinx-metadata's chunked strategy: classifiers first, at most 64 to a
//! fragment (a top-level class counts once with the classes nested in it, a type alias once),
//! then functions and properties, at most 128 to a fragment.
//!
//! A type alias use is written expanded, with the alias as its abbreviation; a type alias's own
//! right-hand side names the aliases it uses directly.

use crate::metadata::protobuf::Pb;
use crate::metadata::type_encoder::StringTable;
use crate::metadata::ClassifierApplications;
use crate::types::TypeName;

use super::kotlin_type::{Argument, Head, KotlinType};
use super::stub::{
    flags, Annotation, Class, Constructor, Function, IntegerKind, Package, Parameter, Property,
    TypeAlias, Value,
};

const CLASSIFIERS_PER_FRAGMENT: usize = 64;
const CALLABLES_PER_FRAGMENT: usize = 128;

/// The fragments of `package` (dotted), in the order the library numbers them.
pub(super) fn fragments(package: &str, contents: &Package) -> Vec<Vec<u8>> {
    let segments: Vec<&str> = package.split('.').filter(|s| !s.is_empty()).collect();
    let classifier_applications = |classifier: TypeName| {
        if classifier.nested_owner().is_none()
            || contents
                .classes
                .iter()
                .flatten()
                .any(|class| class.name == classifier)
        {
            // C declarations nested for namespacing are Kotlin nested classes, not `inner`
            // classes: they capture no enclosing instance or type arguments.
            Ok(crate::metadata::ClassifierApplication::Direct)
        } else {
            Err(classifier)
        }
    };
    enum Classifier<'a> {
        Classes(&'a [Class]),
        Alias(&'a TypeAlias),
    }
    let classifiers: Vec<Classifier<'_>> = contents
        .classes
        .iter()
        .map(|classes| Classifier::Classes(classes))
        .chain(contents.aliases.iter().map(Classifier::Alias))
        .collect();
    let mut fragments = Vec::new();
    for chunk in classifiers.chunks(CLASSIFIERS_PER_FRAGMENT) {
        let mut classes = Vec::new();
        let mut aliases = Vec::new();
        for classifier in chunk {
            match classifier {
                Classifier::Classes(group) => classes.extend(group.iter()),
                Classifier::Alias(alias) => aliases.push(*alias),
            }
        }
        fragments.push(fragment(
            &segments,
            &classifier_applications,
            &aliases,
            &[],
            &[],
            &classes,
        ));
    }
    enum Callable<'a> {
        Function(&'a Function),
        Property(&'a Property),
    }
    let callables: Vec<Callable<'_>> = contents
        .functions
        .iter()
        .map(Callable::Function)
        .chain(contents.properties.iter().map(Callable::Property))
        .collect();
    for chunk in callables.chunks(CALLABLES_PER_FRAGMENT) {
        let mut functions = Vec::new();
        let mut properties = Vec::new();
        for callable in chunk {
            match callable {
                Callable::Function(function) => functions.push(*function),
                Callable::Property(property) => properties.push(*property),
            }
        }
        fragments.push(fragment(
            &segments,
            &classifier_applications,
            &[],
            &functions,
            &properties,
            &[],
        ));
    }
    if fragments.is_empty() {
        fragments.push(fragment(
            &segments,
            &classifier_applications,
            &[],
            &[],
            &[],
            &[],
        ));
    }
    fragments
}

/// The fragment that only names `package`: a parent of the library's package, which declares
/// nothing.
pub(super) fn empty_fragment(package: &str) -> Vec<u8> {
    let mut fragment = Pb::new();
    fragment.field_message(1, &Pb::new()); // PackageFragment.strings = 1
    fragment.field_message(2, &Pb::new()); // PackageFragment.qualified_names = 2
    fragment.field_varint(172, 1); // isEmpty = 172
    fragment.field_bytes(173, package.as_bytes()); // fqName = 173
    fragment.into_bytes()
}

fn fragment(
    package: &[&str],
    classifier_applications: ClassifierApplications<'_>,
    aliases: &[&TypeAlias],
    functions: &[&Function],
    properties: &[&Property],
    classes: &[&Class],
) -> Vec<u8> {
    let mut st = StringTable::klib(classifier_applications);
    let mut package_message = Pb::new();
    st.open_type_table();
    for alias in aliases {
        let message = type_alias(&mut st, alias);
        package_message.repeated_message(5, &message); // Package.type_alias = 5
    }
    for function in functions {
        let message = function_message(&mut st, function);
        package_message.repeated_message(3, &message); // Package.function = 3
    }
    for property in properties {
        let message = property_message(&mut st, property);
        package_message.repeated_message(4, &message); // Package.property = 4
    }
    if let Some(table) = st.close_type_table() {
        package_message.field_message(30, &table); // Package.type_table = 30
    }
    let package_name = st.klib_package(package).map_or(-1_i64, i64::from);
    package_message.field_varint(171, package_name as u64); // packageFqName = 171
    let mut class_messages = Vec::new();
    let mut class_names = Vec::new();
    for class in classes {
        class_messages.push(class_message(&mut st, class));
        class_names.push(u64::from(st.class_id(class.name)));
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
    if !class_names.is_empty() {
        fragment.field_packed_varints(174, &class_names); // className = 174, packed
    }
    fragment.canonical().into_bytes()
}

fn class_message(st: &mut StringTable<'_>, class: &Class) -> Pb {
    let mut message = Pb::new();
    st.open_type_table();
    if let Some(flags) = class.flags {
        message.field_varint(1, flags); // Class.flags = 1
    }
    message.field_varint(3, u64::from(st.class_id(class.name))); // Class.fq_name = 3
    if let Some(companion) = class.companion {
        message.field_varint(4, u64::from(st.local(companion))); // Class.companion_object_name = 4
    }
    let supertypes: Vec<u64> = class
        .supertypes
        .iter()
        .map(|supertype| {
            let ty = expanded(st, supertype);
            u64::from(st.type_id(&ty).expect("a KLIB type table is open"))
        })
        .collect();
    if !supertypes.is_empty() {
        message.field_packed_varints(2, &supertypes); // Class.supertype_id = 2
    }
    let nested: Vec<u64> = class
        .nested
        .iter()
        .map(|name| u64::from(st.local(name)))
        .collect();
    if !nested.is_empty() {
        message.field_packed_varints(7, &nested); // Class.nested_class_name = 7
    }
    for constructor in &class.constructors {
        let constructor = constructor_message(st, constructor);
        message.repeated_message(8, &constructor); // Class.constructor = 8
    }
    for function in &class.functions {
        let function = function_message(st, function);
        message.repeated_message(9, &function); // Class.function = 9
    }
    for property in &class.properties {
        let property = property_message(st, property);
        message.repeated_message(10, &property); // Class.property = 10
    }
    for (ordinal, entry) in class.entries.iter().enumerate() {
        let mut entry_message = Pb::new();
        entry_message.field_varint(1, u64::from(st.local(&entry.name))); // EnumEntry.name = 1
        for annotation in &entry.annotations {
            let annotation = annotation_message(st, annotation);
            entry_message.repeated_message(170, &annotation); // EnumEntry.annotation = 170
        }
        entry_message.field_varint(171, ordinal as u64); // EnumEntry.ordinal = 171
        message.repeated_message(13, &entry_message); // Class.enum_entry = 13
    }
    for annotation in &class.annotations {
        let annotation = annotation_message(st, annotation);
        message.repeated_message(170, &annotation); // Class.annotation = 170
    }
    if let Some(table) = st.close_type_table() {
        message.field_message(30, &table); // Class.type_table = 30
    }
    message
}

fn constructor_message(st: &mut StringTable<'_>, constructor: &Constructor) -> Pb {
    let mut message = Pb::new();
    if let Some(flags) = constructor.flags {
        message.field_varint(1, flags); // Constructor.flags = 1
    }
    for parameter in &constructor.parameters {
        let parameter = parameter_message(st, parameter);
        message.repeated_message(2, &parameter); // Constructor.value_parameter = 2
    }
    message
}

fn function_message(st: &mut StringTable<'_>, function: &Function) -> Pb {
    let mut message = Pb::new();
    message.field_varint(9, function.flags); // Function.flags = 9
    message.field_varint(2, u64::from(st.local(&function.name))); // Function.name = 2
    let result = expanded(st, &function.result);
    st.put_type(&mut message, 3, 7, &result); // Function.return_type(_id) = 3 / 7
    for parameter in &function.parameters {
        let parameter = parameter_message(st, parameter);
        message.repeated_message(6, &parameter); // Function.value_parameter = 6
    }
    for annotation in &function.annotations {
        let annotation = annotation_message(st, annotation);
        message.repeated_message(170, &annotation); // Function.annotation = 170
    }
    message
}

fn parameter_message(st: &mut StringTable<'_>, parameter: &Parameter) -> Pb {
    let mut message = Pb::new();
    if !parameter.annotations.is_empty() {
        message.field_varint(1, flags::ANNOTATED_PARAMETER); // ValueParameter.flags = 1
    }
    message.field_varint(2, u64::from(st.local(&parameter.name))); // ValueParameter.name = 2
    let ty = expanded(st, &parameter.ty);
    st.put_type(&mut message, 3, 5, &ty); // ValueParameter.type(_id) = 3 / 5
    if let Some(element) = &parameter.vararg_element {
        let element = expanded(st, element);
        st.put_type(&mut message, 4, 6, &element); // vararg_element_type(_id) = 4 / 6
    }
    for annotation in &parameter.annotations {
        let annotation = annotation_message(st, annotation);
        message.repeated_message(170, &annotation); // ValueParameter.annotation = 170
    }
    message
}

fn property_message(st: &mut StringTable<'_>, property: &Property) -> Pb {
    let mut message = Pb::new();
    if property.flags != flags::MEMBER_VAL {
        message.field_varint(11, property.flags); // Property.flags = 11
    }
    message.field_varint(2, u64::from(st.local(&property.name))); // Property.name = 2
    let ty = expanded(st, &property.ty);
    st.put_type(&mut message, 3, 9, &ty); // Property.return_type(_id) = 3 / 9
    if property.setter_flags.is_some() {
        let setter = parameter_message(st, &Parameter::new("value", property.ty.clone()));
        message.field_message(6, &setter); // Property.setter_value_parameter = 6
    }
    message.field_varint(7, property.getter_flags); // Property.getter_flags = 7
    if let Some(setter_flags) = property.setter_flags {
        message.field_varint(8, setter_flags); // Property.setter_flags = 8
    }
    for annotation in &property.annotations {
        let annotation = annotation_message(st, annotation);
        message.repeated_message(170, &annotation); // Property.annotation = 170
    }
    if let Some(constant) = &property.constant {
        let value = value_message(st, constant);
        message.field_message(173, &value); // Property.compile_time_value = 173
    }
    for annotation in &property.getter_annotations {
        let annotation = annotation_message(st, annotation);
        message.repeated_message(177, &annotation); // Property.getter_annotation = 177
    }
    for annotation in &property.setter_annotations {
        let annotation = annotation_message(st, annotation);
        message.repeated_message(178, &annotation); // Property.setter_annotation = 178
    }
    message
}

fn type_alias(st: &mut StringTable<'_>, alias: &TypeAlias) -> Pb {
    let mut message = Pb::new();
    message.field_varint(1, flags::TYPE_ALIAS); // TypeAlias.flags = 1
    message.field_varint(2, u64::from(st.local(&alias.name))); // TypeAlias.name = 2
    let underlying = unexpanded(st, &alias.underlying);
    st.put_type(&mut message, 4, 5, &underlying); // TypeAlias.underlying_type(_id) = 4 / 5
    let expanded_type = expanded(st, &alias.underlying);
    st.put_type(&mut message, 6, 7, &expanded_type); // TypeAlias.expanded_type(_id) = 6 / 7
    let annotation = annotation_message(st, &Annotation::experimental());
    message.repeated_message(8, &annotation); // TypeAlias.annotation = 8
    message
}

/// A type as a declaration's type: an alias use expanded, abbreviated by the alias.
fn expanded(st: &mut StringTable<'_>, ty: &KotlinType) -> Pb {
    match &ty.head {
        Head::Class(name) => class_type(st, *name, &ty.arguments, ty.nullable, expanded),
        Head::Alias(alias, expansion) => {
            let mut target = expansion.as_ref();
            let mut nullable = ty.nullable;
            while let Head::Alias(_, inner) = &target.head {
                nullable |= target.nullable;
                target = inner;
            }
            nullable |= target.nullable;
            let Head::Class(class) = target.head else {
                unreachable!("the loop above stops at a class")
            };
            let mut message = class_type(st, class, &target.arguments, nullable, expanded);
            let abbreviation = alias_type(st, *alias, &ty.arguments, ty.nullable, expanded);
            st.put_type(&mut message, 13, 14, &abbreviation); // Type.abbreviated_type(_id) = 13 / 14
            message
        }
    }
}

/// A type as a type alias's right-hand side: an alias use names the alias.
fn unexpanded(st: &mut StringTable<'_>, ty: &KotlinType) -> Pb {
    match &ty.head {
        Head::Class(name) => class_type(st, *name, &ty.arguments, ty.nullable, unexpanded),
        Head::Alias(alias, _) => alias_type(st, *alias, &ty.arguments, ty.nullable, unexpanded),
    }
}

type Encode = fn(&mut StringTable<'_>, &KotlinType) -> Pb;

fn class_type(
    st: &mut StringTable<'_>,
    class: TypeName,
    arguments: &[Argument],
    nullable: bool,
    encode: Encode,
) -> Pb {
    let id = st.class_id(class);
    let mut message = Pb::new();
    write_arguments(st, &mut message, arguments, encode);
    if nullable {
        message.field_varint(3, 1); // Type.nullable = 3
    }
    message.field_varint(6, u64::from(id)); // Type.class_name = 6
    message
}

fn alias_type(
    st: &mut StringTable<'_>,
    alias: TypeName,
    arguments: &[Argument],
    nullable: bool,
    encode: Encode,
) -> Pb {
    let id = st.class_id(alias);
    let mut message = Pb::new();
    write_arguments(st, &mut message, arguments, encode);
    if nullable {
        message.field_varint(3, 1); // Type.nullable = 3
    }
    message.field_varint(12, u64::from(id)); // Type.type_alias_name = 12
    message
}

fn write_arguments(
    st: &mut StringTable<'_>,
    message: &mut Pb,
    arguments: &[Argument],
    encode: Encode,
) {
    for argument in arguments {
        let mut encoded = Pb::new();
        match argument {
            Argument::Star => encoded.field_varint(1, 3), // Projection.STAR
            Argument::Invariant(ty) => {
                let ty = encode(st, ty);
                st.put_type(&mut encoded, 2, 3, &ty); // Argument.type(_id) = 2 / 3
            }
            Argument::Out(ty) => {
                encoded.field_varint(1, 1); // Projection.OUT
                let ty = encode(st, ty);
                st.put_type(&mut encoded, 2, 3, &ty);
            }
        }
        message.repeated_message(2, &encoded); // Type.argument = 2
    }
}

fn annotation_message(st: &mut StringTable<'_>, annotation: &Annotation) -> Pb {
    let mut message = Pb::new();
    message.field_varint(1, u64::from(st.class_id(annotation.class))); // Annotation.id = 1
    for (name, value) in &annotation.arguments {
        let mut argument = Pb::new();
        argument.field_varint(1, u64::from(st.local(name))); // Argument.name_id = 1
        argument.field_message(2, &value_message(st, value)); // Argument.value = 2
        message.repeated_message(2, &argument); // Annotation.argument = 2
    }
    message
}

/// An `Annotation.Argument.Value`: an unsigned integer carries `IS_UNSIGNED` in its flags.
fn value_message(st: &mut StringTable<'_>, value: &Value) -> Pb {
    let mut message = Pb::new();
    match value {
        Value::Integer {
            kind,
            value,
            unsigned,
        } => {
            let ty = match kind {
                IntegerKind::Byte => 0,
                IntegerKind::Short => 2,
                IntegerKind::Int => 3,
                IntegerKind::Long => 4,
            };
            message.field_varint(1, ty); // Value.type = 1
            message.field_varint(2, zigzag(*value)); // Value.int_value = 2
            if *unsigned {
                message.field_varint(10, 1); // Value.flags = 10: IS_UNSIGNED
            }
        }
        Value::Float(value) => {
            message.field_varint(1, 5);
            message.field_fixed32(3, value.to_bits()); // Value.float_value = 3
        }
        Value::Double(value) => {
            message.field_varint(1, 6);
            message.field_fixed64(4, value.to_bits()); // Value.double_value = 4
        }
        Value::String(value) => {
            message.field_varint(1, 8);
            message.field_varint(5, u64::from(st.local(value))); // Value.string_value = 5
        }
        Value::Enum(class, entry) => {
            message.field_varint(1, 10);
            message.field_varint(6, u64::from(st.class_id(*class))); // Value.class_id = 6
            message.field_varint(7, u64::from(st.local(entry))); // Value.enum_value_id = 7
        }
        Value::Annotation(annotation) => {
            message.field_varint(1, 11);
            let annotation = annotation_message(st, annotation);
            message.field_message(8, &annotation); // Value.annotation = 8
        }
        Value::Array(elements) => {
            message.field_varint(1, 12);
            for element in elements {
                let element = value_message(st, element);
                message.repeated_message(9, &element); // Value.array_element = 9
            }
        }
    }
    message
}

fn zigzag(value: i64) -> u64 {
    ((value as u64) << 1) ^ ((value >> 63) as u64)
}
