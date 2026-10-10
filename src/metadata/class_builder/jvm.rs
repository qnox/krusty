//! The JVM carrier: the `@kotlin.Metadata` `d1`/`d2` payload of a class, with the `JvmProtoBuf`
//! signature, module-name, local-variable and class-flag extensions.

use crate::metadata::local_properties::{
    local_property_pb, LocalPropertyMeta, LOCAL_VARIABLE_FIELD,
};
use crate::metadata::protobuf::Pb;
use crate::metadata::type_encoder::{StringTable, TypeParameters};
use crate::types::{Ty, TypeName};

use super::carrier::{AnnotationSite, ClassCarrier, ConstructorSlot};
use super::declarations::{ClassTail, EnumEntryMeta, FnMeta, PropMeta};
use super::schema::{class_message, ClassDeclaration};

/// How the JVM realizes one class's declarations, as its `@kotlin.Metadata` names them. Each list is
/// parallel to the declaration's own: a property or function past its end records nothing.
#[derive(Default)]
pub struct JvmClassSignatures<'a> {
    /// The `-module-name` value → `JvmProtoBuf.classModuleName` (f101). kotlinc omits it for the
    /// default module `main`; downstream builds always set `-module-name`.
    pub module_name: Option<&'a str>,
    /// `JvmProtoBuf.jvmClassFlags` (f104) — kotlinc emits `3` for an interface under
    /// `-jvm-default=all`. `None` omits the field.
    pub class_flags: Option<u64>,
    /// The class's local delegated properties, in `<v#N>` order (`JvmProtoBuf.classLocalVariable`).
    pub local_properties: &'a [LocalPropertyMeta],
    /// The primary constructor's realization. `None` records none: an annotation class's
    /// classfile is an annotation interface with no `<init>`.
    pub primary_constructor: Option<JvmConstructorSignature>,
    /// The secondary constructors' realizations, parallel to the declaration's.
    pub secondary_constructors: Vec<JvmConstructorSignature>,
    /// The properties' realizations, parallel to the declaration's.
    pub properties: Vec<JvmPropertySignature>,
    /// The functions' realizations, parallel to the declaration's.
    pub functions: Vec<JvmFunctionSignature>,
}

/// A constructor's `JvmMethodSignature`: `<init>` or a value class's static `constructor-impl`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JvmConstructorSignature {
    pub name: String,
    pub desc: String,
}

impl JvmConstructorSignature {
    /// An ordinary `<init>` with descriptor `desc`.
    pub fn init(desc: impl Into<String>) -> Self {
        JvmConstructorSignature {
            name: "<init>".into(),
            desc: desc.into(),
        }
    }
}

/// A function's `JvmMethodSignature`, recorded only for the parts a reader cannot rebuild from the
/// declared types: a realized name differing from the declared one, and its descriptor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JvmFunctionSignature {
    pub name: Option<String>,
    pub desc: Option<String>,
}

/// A property's `JvmPropertySignature`: its backing field, annotation marker and accessors.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JvmPropertySignature {
    /// `(jvm name, jvm descriptor)` of each accessor the class declares.
    pub getter: Option<(String, String)>,
    pub setter: Option<(String, String)>,
    /// The backing field, when the property owns one. An abstract or computed property has none,
    /// and kotlinc then omits the entry rather than writing an empty one.
    pub field: Option<JvmFieldSignature>,
    /// `(name, descriptor)` of the `get<Name>$annotations()` marker method carrying the property's
    /// annotations — a Kotlin property has no class-file declaration of its own. `None` when the
    /// property has no property-targeted annotation.
    pub synthetic_method: Option<(String, String)>,
    /// kotlinc's `JvmFlags.IS_MOVED_FROM_INTERFACE_COMPANION` (`Property` extension f101 = 1): a
    /// `@JvmField` property of an INTERFACE's companion, whose backing field was hoisted onto the
    /// interface itself.
    pub moved_from_interface_companion: bool,
}

/// A backing field's `JvmFieldSignature`; each part is recorded only when a reader cannot derive it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JvmFieldSignature {
    /// The physical name when it differs from the property's (`result` → `result$1`).
    pub name: Option<String>,
    /// The descriptor when it is not the property type's (kotlinc's `requiresSignature`): a type
    /// parameter, a boxed nullable primitive, a value class's erased underlying, a reference array,
    /// or a delegate of another type.
    pub desc: Option<String>,
}

/// The `@kotlin.Metadata` carrier.
struct JvmCarrier<'a, 'b> {
    signatures: &'b JvmClassSignatures<'a>,
}

static NO_PROPERTY_SIGNATURE: JvmPropertySignature = JvmPropertySignature {
    getter: None,
    setter: None,
    field: None,
    synthetic_method: None,
    moved_from_interface_companion: false,
};

impl ClassCarrier for JvmCarrier<'_, '_> {
    fn annotation_field(&self, site: AnnotationSite) -> u32 {
        match site {
            AnnotationSite::Class => 25,
            AnnotationSite::Constructor => 3,
            AnnotationSite::ValueParameter => 7,
            AnnotationSite::EnumEntry => 2,
            AnnotationSite::Function => 12,
            AnnotationSite::Property => 14,
            AnnotationSite::Getter => 15,
            AnnotationSite::Setter => 16,
            AnnotationSite::BackingField => 34,
        }
    }

    fn constructor_extensions(&self, st: &mut StringTable<'_>, constructor: ConstructorSlot) -> Pb {
        let signature = match constructor {
            ConstructorSlot::Primary => self.signatures.primary_constructor.as_ref(),
            ConstructorSlot::Secondary(index) => self.signatures.secondary_constructors.get(index),
        };
        let mut extensions = Pb::new();
        if let Some(signature) = signature {
            let signature = jvm_method_sig(st, Some(&signature.name), &signature.desc);
            extensions.field_message(100, &signature); // JvmProtoBuf.constructorSignature = 100
        }
        extensions
    }

    fn property_extensions(&self, st: &mut StringTable<'_>, index: usize) -> Pb {
        let property = self
            .signatures
            .properties
            .get(index)
            .unwrap_or(&NO_PROPERTY_SIGNATURE);
        let mut extensions = Pb::new();
        let signature = property_jvm_signature(st, property);
        extensions.field_message(100, &signature); // JvmProtoBuf.propertySignature = 100
        if property.moved_from_interface_companion {
            extensions.field_varint(101, 1); // JvmProtoBuf.flags = 101: IS_MOVED_FROM_INTERFACE_COMPANION
        }
        extensions
    }

    fn property_trailer(&self, _: &mut StringTable<'_>, _: usize) -> Pb {
        Pb::new()
    }

    fn function_extensions(&self, st: &mut StringTable<'_>, index: usize) -> Pb {
        let mut extensions = Pb::new();
        let Some(function) = self.signatures.functions.get(index) else {
            return extensions;
        };
        if function.desc.is_some() || function.name.is_some() {
            let mut signature = Pb::new();
            if let Some(name) = function.name.as_deref() {
                signature.field_varint(1, st.local(name) as u64); // JvmMethodSignature.name = 1
            }
            if let Some(desc) = function.desc.as_deref() {
                signature.field_varint(2, st.local(desc) as u64); // JvmMethodSignature.desc = 2
            }
            extensions.field_message(100, &signature); // JvmProtoBuf.methodSignature = 100
        }
        extensions
    }

    fn function_trailer(&self, _: &mut StringTable<'_>, _: &FnMeta) -> Pb {
        Pb::new()
    }

    fn class_extensions(&self, st: &mut StringTable<'_>, type_parameters: &TypeParameters) -> Pb {
        let mut extensions = Pb::new();
        // The module name interns after every structural string, the local delegated properties
        // after it.
        if let Some(module) = self.signatures.module_name {
            extensions.field_varint(101, u64::from(st.local(module))); // JvmProtoBuf.classModuleName
        }
        for property in self.signatures.local_properties {
            let local = local_property_pb(st, property, type_parameters);
            extensions.repeated_message(LOCAL_VARIABLE_FIELD, &local); // classLocalVariable
        }
        if let Some(flags) = self.signatures.class_flags {
            extensions.field_varint(104, flags); // JvmProtoBuf.classFlags = 104 (interfaces carry 3)
        }
        extensions
    }

    fn class_trailer(&self, _: &mut StringTable<'_>) -> Pb {
        Pb::new()
    }
}

/// `JvmProtoBuf.propertySignature` (100): the backing field, the annotation marker and the
/// accessors a property is realized with on the JVM.
pub(super) fn property_jvm_signature(st: &mut StringTable<'_>, p: &JvmPropertySignature) -> Pb {
    let mut jvm = Pb::new();
    // kotlinc interns the getter/setter strings BEFORE the field's (even though the proto
    // writes `field` (f1) first), so build them in that order.
    // The marker interns BEFORE the getter, exactly as it serializes (f2 before f3).
    let synthetic_method = p
        .synthetic_method
        .as_ref()
        .map(|(name, desc)| jvm_method_sig(st, Some(name), desc));
    let getter = p
        .getter
        .as_ref()
        .map(|(gn, gd)| jvm_method_sig(st, Some(gn), gd));
    let setter = p
        .setter
        .as_ref()
        .map(|(sn, sd)| jvm_method_sig(st, Some(sn), sd));
    // An abstract property has no backing field at all: kotlinc omits the entry rather than
    // writing an empty one, and interns none of its strings.
    let field = p.field.as_ref().map(|signature| {
        let mut field = Pb::new();
        if let Some(n) = &signature.name {
            field.field_varint(1, st.local(n) as u64); // JvmFieldSignature.name = 1
        }
        if let Some(d) = &signature.desc {
            field.field_varint(2, st.local(d) as u64); // JvmFieldSignature.desc = 2
        }
        field
    });
    if let Some(field) = &field {
        jvm.field_message(1, field); // field (empty → derived)
    }
    if let Some(synthetic_method) = &synthetic_method {
        jvm.field_message(2, synthetic_method); // JvmPropertySignature.syntheticMethod = 2
    }
    if let Some(getter) = &getter {
        jvm.field_message(3, getter); // JvmPropertySignature.getter = 3
    }
    if let Some(setter) = &setter {
        jvm.field_message(4, setter); // JvmPropertySignature.setter = 4
    }
    jvm
}

pub(super) fn jvm_method_sig(st: &mut StringTable<'_>, name: Option<&str>, desc: &str) -> Pb {
    let mut p = Pb::new();
    if let Some(n) = name {
        p.field_varint(1, st.local(n) as u64); // JvmMethodSignature.name = 1
    }
    p.field_varint(2, st.local(desc) as u64); // JvmMethodSignature.desc = 2
    p
}

/// Build `(d1 bytes, d2 strings)` for a class. `class_internal` is e.g. `demo/Point`;
/// `ctor_params` are the primary-constructor `(name, type)` pairs; `signatures` how the JVM
/// realizes the declarations.
pub fn build_class(
    class_internal: TypeName,
    ctor_params: &[(String, Ty)],
    props: &[PropMeta],
    methods: &[FnMeta],
    enum_entries: &[EnumEntryMeta<'_>],
    tail: &ClassTail,
    signatures: &JvmClassSignatures<'_>,
) -> (Vec<u8>, Vec<String>) {
    let mut st = StringTable::with_local_classifiers(
        tail.local_classifiers,
        tail.enum_entry_bodies,
        tail.intersection_approximation,
    );
    let class = class_message(
        &mut st,
        &ClassDeclaration {
            name: class_internal,
            ctor_params,
            props,
            methods,
            enum_entries,
            tail,
        },
        &JvmCarrier { signatures },
    );
    let stt = st.serialize_types();
    let mut bytes = vec![0x00u8]; // UTF8 mode marker
    let mut prefix = Pb::new();
    prefix.varint(stt.as_bytes().len() as u64); // writeDelimitedTo length prefix
    bytes.extend_from_slice(&prefix.into_bytes());
    bytes.extend_from_slice(stt.as_bytes());
    bytes.extend_from_slice(class.as_bytes());
    (bytes, st.into_strings())
}
