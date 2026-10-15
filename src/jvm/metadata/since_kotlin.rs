//! API-version gates on a decoded declaration. Kotlinc publishes the versions as annotation
//! arguments: `@SinceKotlin` admits a declaration from a version onward, while
//! `@DeprecatedSinceKotlin(hiddenSince = ...)` removes it at that version.

use crate::language_version::LanguageVersion;
use crate::metadata::decode::Pb;
use crate::types::{type_name, TypeName};

use super::string_table::{resolve_class_name, resolve_string, Rec};
use super::{MetaConstructor, MetaFn, MetaProp};

/// Declaration availability carried by exact JVM realization methods. Properties use their
/// metadata-recorded annotation method and constructors use their metadata-recorded `<init>`
/// descriptor; source spelling is never an association key.
pub(super) struct RealizationAvailability<'a> {
    methods: &'a [crate::jvm::classreader::MethodSig],
}

impl<'a> RealizationAvailability<'a> {
    pub(super) fn new(methods: &'a [crate::jvm::classreader::MethodSig]) -> Self {
        Self { methods }
    }

    fn method(
        &self,
        jvm_name: &str,
        jvm_desc: Option<&str>,
    ) -> Option<&'a crate::jvm::classreader::MethodSig> {
        let descriptor = jvm_desc?;
        self.methods
            .iter()
            .find(|method| method.name == jvm_name && method.descriptor == descriptor)
    }

    pub(super) fn functions(&self, mut declarations: Vec<MetaFn>) -> Vec<MetaFn> {
        for declaration in &mut declarations {
            if self
                .method(&declaration.jvm_name, declaration.jvm_desc)
                .is_some_and(|method| method.deprecated_hidden)
            {
                declaration.flags = declaration.flags.with_deprecated_hidden(true);
            }
        }
        declarations
    }

    pub(super) fn properties(&self, mut declarations: Vec<MetaProp>) -> Vec<MetaProp> {
        for declaration in &mut declarations {
            if let Some(method) = declaration
                .annotation_method
                .as_ref()
                .and_then(|method| self.method(&method.name, Some(method.desc.as_str())))
            {
                declaration.deprecated_hidden = method.deprecated_hidden;
                declaration.hidden_since = method.deprecated_hidden_since;
            }
        }
        declarations
    }

    pub(super) fn constructors(&self, declarations: &mut [MetaConstructor]) {
        for declaration in declarations {
            if let Some(method) = self.method(declaration.jvm_name, declaration.jvm_desc) {
                declaration.deprecated_hidden = method.deprecated_hidden;
                declaration.hidden_since = method.deprecated_hidden_since;
            }
        }
    }
}

pub(super) fn annotation_names(
    bodies: &[Vec<u8>],
    records: &[Rec],
    d2: &[String],
) -> Vec<TypeName> {
    bodies
        .iter()
        .filter_map(|body| {
            let mut pb = Pb::new(body);
            let mut id = None;
            while !pb.at_end() {
                let tag = pb.varint()?;
                match (tag >> 3, tag & 7) {
                    (1, 0) => id = pb.varint(),
                    (_, wire) => pb.skip(wire)?,
                }
            }
            id.and_then(|id| resolve_class_name(records, d2, id as usize))
                .map(|name| type_name(&name))
        })
        .collect()
}

/// The `version` argument of `@kotlin.SinceKotlin`, when this declaration carries that annotation.
pub(super) fn version(
    bodies: &[Vec<u8>],
    records: &[Rec],
    d2: &[String],
) -> Option<LanguageVersion> {
    version_argument_of(bodies, records, d2, "kotlin/SinceKotlin", "version")
}

/// The `hiddenSince` argument of `@kotlin.DeprecatedSinceKotlin`, when present.
pub(super) fn hidden_since(
    bodies: &[Vec<u8>],
    records: &[Rec],
    d2: &[String],
) -> Option<LanguageVersion> {
    version_argument_of(
        bodies,
        records,
        d2,
        "kotlin/DeprecatedSinceKotlin",
        "hiddenSince",
    )
}

fn version_argument_of(
    bodies: &[Vec<u8>],
    records: &[Rec],
    d2: &[String],
    annotation: &str,
    parameter: &str,
) -> Option<LanguageVersion> {
    for body in bodies {
        let mut pb = Pb::new(body);
        let mut id = None;
        let mut since = None;
        while !pb.at_end() {
            let Some(tag) = pb.varint() else {
                break;
            };
            match (tag >> 3, tag & 7) {
                (1, 0) => id = pb.varint(),
                (2, 2) => {
                    let Some(len) = pb.varint() else { break };
                    let Some(argument) = pb.bytes(len as usize) else {
                        break;
                    };
                    if let Some(text) = version_argument(argument, records, d2, parameter) {
                        since = LanguageVersion::from_major_minor_text(&text);
                    }
                }
                (_, wire) => {
                    if pb.skip(wire).is_none() {
                        break;
                    }
                }
            }
        }
        let matches_annotation = id
            .and_then(|id| resolve_class_name(records, d2, id as usize))
            .as_deref()
            == Some(annotation);
        if matches_annotation {
            return since;
        }
    }
    None
}

/// `Argument { name_id = 1, value = 2: Value { string_value = 5 } }` for `parameter`.
fn version_argument(
    argument: &[u8],
    records: &[Rec],
    d2: &[String],
    parameter: &str,
) -> Option<String> {
    let mut pb = Pb::new(argument);
    let mut name = None;
    let mut value = None;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => {
                let id = pb.varint()?;
                name = resolve_string(records, d2, id as usize);
            }
            (2, 2) => {
                let len = pb.varint()? as usize;
                let body = pb.bytes(len)?;
                value = string_value(body);
            }
            (_, wire) => pb.skip(wire)?,
        }
    }
    (name.as_deref() == Some(parameter))
        .then(|| value.and_then(|id| resolve_string(records, d2, id as usize)))
        .flatten()
}

fn string_value(body: &[u8]) -> Option<u64> {
    let mut pb = Pb::new(body);
    let mut value = None;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (5, 0) => value = pb.varint(),
            (_, wire) => pb.skip(wire)?,
        }
    }
    value
}
