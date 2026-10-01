//! Kotlin metadata typealias declarations and their source-level spelling templates.

use super::string_table::{resolve_class_name, resolve_string, Rec};
use super::{
    decode_metadata_type, flags_visibility, parse_type_class_name, type_param_bodies, MetaCtx,
    MetadataResult, TYPE_ALIAS_TYPE_PARAMETER_FIELD, VIS_PUBLIC,
};
use crate::metadata::decode::{parse_type_node, parse_type_param, ParsedTypeParam, Pb};
use crate::types::{type_name, Ty, TypeName};
use std::collections::HashMap;

/// One public `typealias` from a file facade's `Package` metadata.
#[derive(Clone, Debug)]
pub struct MetaTypeAlias {
    /// The alias's declaring identity (`package/Name`, or the classifier owner's path plus
    /// `/Name`). The classpath index keys this identity; it is not a rendered spelling.
    pub name: TypeName,
    /// The expanded target's class internal name.
    pub target: String,
    /// The alias's own type-parameter names, in declaration order — the substitution domain.
    pub formals: Vec<String>,
    /// The target applied to its own arguments, with the alias's parameters as `Ty::TyParam`.
    /// Metadata decoding is authoritative: an alias is not published when this type cannot decode.
    pub expansion: Ty,
    /// How the right-hand side SPELLED the arguments it passes to its target — see
    /// [`crate::spelling`]. A consuming module inherits these into every use site's expansion.
    pub expansion_spelling: crate::spelling::Spelled,
}

/// Type aliases declared in a file facade's `Package` `@Metadata` (`typealias Alias = Real` →
/// `("Alias", "pkg/Real")`). Reads the `Package.typeAlias` entries (field 5) from the proto
/// directly: each alias's name (field 2, a string-table id) and its EXPANDED type (field 6, fully
/// resolved to the concrete class, so an alias chain collapses to the final class; falls back to
/// the immediate underlying type, field 4). This is robust where the older `d2` `$annotations`
/// heuristic was not — a file facade also carries annotated top-level properties whose
/// `$annotations` markers that heuristic would misread as aliases.
pub(super) fn decode_type_aliases(
    ctx: &MetaCtx,
    package: Option<&str>,
    this_class: &str,
    classifier_owner: bool,
) -> MetadataResult<Vec<MetaTypeAlias>> {
    let owner = declaring_owner(package, this_class, classifier_owner);
    let field = if classifier_owner { 11 } else { 5 };
    let mut out = Vec::new();
    let records = ctx.records;
    let d2 = ctx.d2;
    let mut pb = Pb::new(ctx.msg);
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (candidate, 2) if candidate == field => {
                let Some(len) = pb.varint() else { break };
                let Some(body) = pb.bytes(len as usize) else {
                    break;
                };
                if let Some(alias) = parse_type_alias(owner, body, records, d2)? {
                    // Key the alias by its FULL internal name — its DECLARING package plus the
                    // alias's simple name — so `kotlin/collections/ArrayList` is distinct from any
                    // other package's `ArrayList`. `resolve_type` looks it up by that identity, and
                    // an `import kotlin.test.Test` spells the DECLARED package, never the relocated
                    // one.
                    out.push(alias);
                }
            }
            (_, wire) => {
                if pb.skip(wire).is_none() {
                    break;
                }
            }
        }
    }
    Ok(out)
}

/// The semantic declaration owner of a package or classifier metadata typealias table.
fn declaring_owner(package: Option<&str>, this_class: &str, classifier_owner: bool) -> TypeName {
    // `@Metadata`'s `pn` is authoritative when `@JvmPackageName` moved a facade out of its source
    // package (`package kotlin.test` emitted under `kotlin/test/junit5/...`). Otherwise the class's
    // JVM package is the declaring package.
    let package = package.unwrap_or_else(|| this_class.rsplit_once('/').map_or("", |(p, _)| p));
    if !classifier_owner {
        return type_name(package);
    }

    // A classifier's classfile identity uses `$` for nested ownership. Typealias declarations live
    // in the source classifier namespace, so retain the already-parsed nested segments and publish
    // `pkg/Outer/Inner`, never a manufactured `pkg/Outer$Inner/Alias` classifier identity.
    let classifier = type_name(this_class);
    let mut segments = Vec::new();
    let mut current = classifier;
    while let Some(parent) = current.nested_owner() {
        segments.push(current.nested_segment_ref());
        current = parent;
    }
    segments.push(current.segment_ref());
    segments.reverse();
    let mut owner = classifier.namespace();
    for segment in segments {
        owner = crate::types::type_name_child(owner, segment);
    }
    owner
}

/// Decode a public `TypeAlias` message → its name, the expanded/underlying class internal name,
/// and the EXPANSION TEMPLATE: the target applied to its own arguments, with the alias's parameters
/// left as `Ty::TyParam`. `typealias Lens<S, A> = PLens<S, S, A, A>` declares two parameters for a
/// four-parameter target, so a use site's arguments must be substituted into the template rather
/// than pasted onto the target — the template is the only place that mapping exists.
fn parse_type_alias(
    owner: TypeName,
    body: &[u8],
    records: &[Rec],
    d2: &[String],
) -> MetadataResult<Option<MetaTypeAlias>> {
    let type_parameters = type_param_bodies(body, TYPE_ALIAS_TYPE_PARAMETER_FIELD)
        .into_iter()
        .map(parse_type_param)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(parse_type_alias_decoded(
        owner,
        body,
        records,
        d2,
        type_parameters,
    ))
}

fn parse_type_alias_decoded(
    owner: TypeName,
    body: &[u8],
    records: &[Rec],
    d2: &[String],
    type_parameters: Vec<ParsedTypeParam>,
) -> Option<MetaTypeAlias> {
    let mut pb = Pb::new(body);
    let mut flags = 6u64;
    let mut name_id: Option<u64> = None;
    let mut expanded_class: Option<u64> = None;
    let mut underlying_class: Option<u64> = None;
    let mut expanded_body: Option<&[u8]> = None;
    let mut underlying_body: Option<&[u8]> = None;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (1, 0) => flags = pb.varint()?,
            (2, 0) => name_id = pb.varint(),
            (4, 2) => {
                let len = pb.varint()? as usize;
                let type_body = pb.bytes(len)?;
                underlying_class = parse_type_class_name(type_body);
                underlying_body = Some(type_body);
            }
            (6, 2) => {
                let len = pb.varint()? as usize;
                let type_body = pb.bytes(len)?;
                expanded_class = parse_type_class_name(type_body);
                expanded_body = Some(type_body);
            }
            (_, wire) => pb.skip(wire)?,
        }
    }
    if flags_visibility(flags) != VIS_PUBLIC {
        return None;
    }
    let simple_name = d2.get(name_id? as usize)?;
    let name = crate::types::type_name_child(owner, simple_name);
    let class_id = expanded_class.or(underlying_class)?;
    let internal = resolve_class_name(records, d2, class_id as usize)?;
    // The alias's OWN type parameters, by metadata id, so the expansion decodes their uses as
    // `TyParam` rather than as unknown classifiers.
    let mut parameters: HashMap<u64, String> = HashMap::new();
    let mut formals = Vec::new();
    for parameter in type_parameters {
        if let Some(parameter_name) = resolve_string(records, d2, parameter.name_id as usize) {
            parameters.insert(parameter.id, parameter_name.clone());
            formals.push(parameter_name);
        }
    }
    let expansion = expanded_body.or(underlying_body).and_then(|type_body| {
        decode_metadata_type(
            type_body,
            None,
            records,
            d2,
            &parameters,
            &HashMap::new(),
            false,
            0,
        )
    })?;
    // How the right-hand side SPELLED its arguments, so a use site in another module inherits the
    // abbreviation (`typealias CargoBox = PBox<Cargo, Cargo>` abbreviates both expanded arguments).
    let expansion_spelling = expanded_body
        .map(|type_body| crate::spelling::Spelled {
            definitely_non_null: parse_type_node(type_body)
                .is_some_and(|node| node.definitely_non_null),
            alias: None,
            alias_args: Vec::new(),
            args: parse_type_argument_spellings(type_body, records, d2),
        })
        .unwrap_or_default();
    Some(MetaTypeAlias {
        name,
        target: internal,
        formals,
        expansion,
        expansion_spelling,
    })
}

/// The `typealias` spellings recorded on a `Type`'s arguments as `Type.abbreviated_type`.
fn parse_type_argument_spellings(
    body: &[u8],
    records: &[Rec],
    d2: &[String],
) -> Vec<crate::spelling::Spelled> {
    let mut arguments = Vec::new();
    let mut pb = Pb::new(body);
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            // Type.argument = 2, each an `Argument { projection = 1, type = 2 }`.
            (2, 2) => {
                let Some(len) = pb.varint() else { break };
                let Some(argument) = pb.bytes(len as usize) else {
                    break;
                };
                arguments.push(parse_argument_spelling(argument, records, d2));
            }
            (_, wire) => {
                if pb.skip(wire).is_none() {
                    break;
                }
            }
        }
    }
    if arguments.iter().all(crate::spelling::Spelled::is_none) {
        return Vec::new();
    }
    arguments
}

/// One `Argument`'s spelling: its inner `Type`'s own abbreviation plus, recursively, its arguments'.
fn parse_argument_spelling(
    body: &[u8],
    records: &[Rec],
    d2: &[String],
) -> crate::spelling::Spelled {
    let mut pb = Pb::new(body);
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (2, 2) => {
                let Some(len) = pb.varint() else { break };
                let Some(inner) = pb.bytes(len as usize) else {
                    break;
                };
                return crate::spelling::Spelled {
                    definitely_non_null: parse_type_node(inner)
                        .is_some_and(|node| node.definitely_non_null),
                    alias: parse_type_alias_name(inner, records, d2),
                    alias_args: Vec::new(),
                    args: parse_type_argument_spellings(inner, records, d2),
                };
            }
            (_, wire) => {
                if pb.skip(wire).is_none() {
                    break;
                }
            }
        }
    }
    crate::spelling::Spelled::default()
}

/// The alias a `Type` was spelled as: its `abbreviated_type` (f13) and that message's
/// `type_alias_name` (f12), resolved to a fully-qualified identity.
fn parse_type_alias_name(body: &[u8], records: &[Rec], d2: &[String]) -> Option<TypeName> {
    let mut pb = Pb::new(body);
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (13, 2) => {
                let len = pb.varint()? as usize;
                let abbreviated = pb.bytes(len)?;
                let mut inner = Pb::new(abbreviated);
                while !inner.at_end() {
                    let tag = inner.varint()?;
                    match (tag >> 3, tag & 7) {
                        (12, 0) => {
                            let id = inner.varint()?;
                            return resolve_class_name(records, d2, id as usize)
                                .map(|name| type_name(&name));
                        }
                        (_, wire) => inner.skip(wire)?,
                    }
                }
                return None;
            }
            (_, wire) => pb.skip(wire)?,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const OMITTED_FLAGS: [u8; 6] = [0x10, 0x00, 0x32, 0x02, 0x30, 0x01];
    const INTERNAL_FLAGS: [u8; 8] = [0x08, 0x00, 0x10, 0x00, 0x32, 0x02, 0x30, 0x01];

    fn alias(owner: TypeName, body: &[u8]) -> Option<MetaTypeAlias> {
        parse_type_alias(
            owner,
            body,
            &[],
            &["Alias".to_string(), "sample/Real".to_string()],
        )
        .expect("valid alias metadata")
    }

    #[test]
    fn visibility_and_package_owner_are_semantic_identities() {
        let public = alias(TypeName::ROOT, &OMITTED_FLAGS).expect("public alias decodes");
        assert_eq!(public.name, type_name("Alias"));
        let packaged = alias(type_name("sample"), &OMITTED_FLAGS).expect("public alias decodes");
        assert_eq!(packaged.name, type_name("sample/Alias"));
        assert_eq!(public.target, "sample/Real");
        assert!(public.formals.is_empty());
        assert_eq!(public.expansion, Ty::obj("sample/Real"));
        assert!(alias(TypeName::ROOT, &INTERNAL_FLAGS).is_none());
    }

    #[test]
    fn nested_classifier_owner_uses_source_identity_not_classfile_spelling() {
        let owner = declaring_owner(None, "sample/Outer$Middle$Inner", true);
        assert_eq!(owner, type_name("sample/Outer/Middle/Inner"));
        let decoded = alias(owner, &OMITTED_FLAGS).expect("public nested alias decodes");
        assert_eq!(decoded.name, type_name("sample/Outer/Middle/Inner/Alias"));
    }
}
