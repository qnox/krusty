//! Reading a build file into a [`Node`] tree against the schema, with the toolchain's messages and
//! recovery (`frontend/schema/.../tree/reading`). A value that cannot be read is reported and
//! becomes [`Value::Error`], so its property falls back to its default; reading goes on, so every
//! problem in a file is reported.

use std::path::{Component, Path, PathBuf};

use super::contexts::Contexts;
use super::node::{Entry, FileId, Node, Trace, Value};
use crate::diagnostic::{Diagnostic, Diagnostics, Severity};
use crate::reading::{FileReader, Value as Yaml};
use crate::schema::dependency_types::{
    BOM, CATALOG, EXTERNAL_MAVEN, INTERNAL, UNSCOPED_BOM, UNSCOPED_CATALOG,
    UNSCOPED_EXTERNAL_MAVEN, UNSCOPED_MODULE,
};
use crate::schema::{render, DependencyKind, EnumType, ObjectType, Property, Type};
use crate::yaml::{Document, NodeId, NodeKind, Position, Style};

const WRONG_SCOPED: &str = "Wrong scoped dependency syntax. Possible syntax includes: \n1. `<external-notation>` - a string with the format `<groupId>:<artifactId>:<version>` \n2. `<local-notation>` - a path to another module (starting with the `//` for paths relative to the project root, e.g. `//foo`, or with the `.` for paths relative to the containing directory, e.g. `../foo`) \n3. `<catalog-notation>` - a special reference to the version catalog, starting with the `$` symbol, e.g. `$libs.foo` \n4. `bom: <catalog-notation> | <external-notation>` - BOM dependency \n Additional dependency attributes (not for BOM) can be customized after the `:`, making it a single key-value mapping, e.g., `<external-notation>: exported` or `<local-notation>: compile-only`, etc. A full form may also be used, e.g. `<notation>: '{' exported: true, scope: compile-only '}'`";
const WRONG_UNSCOPED: &str = "Wrong dependency syntax. Possible syntax includes: \n1. `<external-notation>` - a string with the format `<groupId>:<artifactId>:<version>` \n2. `<local-notation>` - a path to another module (starting with the `//` for paths relative to the project root, e.g. `//foo`, or with the `.` for paths relative to the containing directory, e.g. `../foo`) \n3. `<catalog-notation>` - a special reference to the version catalog, starting with the `$` symbol, e.g. `$libs.foo` \n4. `bom: <external-notation> | <catalog-notation>` - BOM dependency \nDependency scope (`exported`, `compile-only`, etc.) is not applicable here.";
const WRONG_UNSCOPED_EXTERNAL: &str = "Wrong dependency syntax. Possible syntax includes: \n1. `<external-notation>` - a string with the format `<groupId>:<artifactId>:<version>` \n2. `<catalog-notation>` - a special reference to the version catalog, starting with the `$` symbol, e.g. `$libs.foo` \nDependency scope (`exported`, `compile-only`, etc.) is not applicable here.";
const REFERENCE: &str =
    "References are not yet supported in this file. The string is interpreted literally";
const REFERENCE_KEY: &str =
    "References are not yet supported in the mapping keys. The string is interpreted literally";

/// Where a file's relative paths resolve.
pub struct Paths<'a> {
    /// The project root, for `//` paths.
    pub root: &'a Path,
    /// The file's directory, for other relative paths.
    pub base: &'a Path,
}

/// Read `document`, the file `file` (at `path`), as `object`. Its values are specific to `file`.
pub fn read(
    path: &Path,
    file: FileId,
    document: &Document,
    object: &'static ObjectType,
    paths: Paths<'_>,
    diagnostics: &mut Diagnostics,
) -> Node {
    let contexts = Contexts {
        file: Some(file),
        ..Contexts::default()
    };
    let mut parser = Parser {
        reader: FileReader::new(path, document, diagnostics),
        file,
        paths,
    };
    let tree = match document.root() {
        Some(root) => parser.node(root, &Type::Object(object), false, contexts),
        None => Node::new(
            Value::Mapping {
                object: Some(object),
                entries: Vec::new(),
            },
            Trace::File {
                file,
                position: Position { line: 1, column: 1 },
            },
            contexts,
        ),
    };
    report_unknown_properties(&tree, path, diagnostics);
    report_deprecations(&tree, path, diagnostics);
    tree
}

struct Parser<'a, 'p> {
    reader: FileReader<'a>,
    file: FileId,
    paths: Paths<'p>,
}

/// How a dependency's shape reads (`DependencyTypeInferenceResult`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Notation {
    Local,
    Catalog,
    Maven,
    Bom,
    SwiftPackage,
    Failed,
}

fn notation_of_text(text: &str) -> Notation {
    match text.chars().next() {
        Some('$') => Notation::Catalog,
        Some('.' | '/') => Notation::Local,
        _ if matches!(text, "localSwiftPackage" | "swiftPackage") => Notation::SwiftPackage,
        _ => Notation::Maven,
    }
}

fn contains_reference(text: &str) -> bool {
    text.contains("${")
}

/// A scalar type the toolchain reports `null` on with its "use quotes" hint.
fn string_like(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Enum(_) | Type::Path | Type::String | Type::NonBlankString | Type::MainClass
    )
}

impl Parser<'_, '_> {
    fn trace(&self, node: NodeId) -> Trace {
        Trace::File {
            file: self.file,
            position: self.reader.position(node),
        }
    }

    fn report(&mut self, position: Position, severity: Severity, message: impl Into<String>) {
        let diagnostic = match severity {
            Severity::Error => Diagnostic::error(&self.reader.file, Some(position), message),
            severity => Diagnostic::warning(severity, &self.reader.file, Some(position), message),
        };
        self.reader.diagnostics.push(diagnostic);
    }

    fn error(&mut self, node: NodeId, message: impl Into<String>) {
        self.reader.error(node, message);
    }

    fn error_node(&self, node: NodeId, contexts: Contexts) -> Node {
        Node::new(Value::Error, self.trace(node), contexts)
    }

    /// A tag the schema does not use (`!!` tags are refused when the file is parsed).
    fn refuse_tag(&mut self, node: NodeId) {
        self.reader.meet(node);
    }

    fn is_plain(&self, node: NodeId) -> bool {
        matches!(
            self.reader.document.node(node).kind,
            NodeKind::Scalar {
                style: Style::Plain,
                ..
            }
        )
    }

    /// `parseNode`: any value, against `ty`.
    fn node(&mut self, node: NodeId, ty: &Type, nullable: bool, contexts: Contexts) -> Node {
        let object_like = matches!(ty, Type::Object(_) | Type::Dependency(_));
        if !object_like && !matches!(ty, Type::Opaque) {
            self.refuse_tag(node);
        }
        if matches!(ty, Type::Opaque) {
            return Node::new(Value::Opaque, self.trace(node), contexts);
        }
        if let Yaml::Null = self.reader.value(node) {
            if nullable {
                return Node::new(Value::Null, self.trace(node), contexts);
            }
            let message = if string_like(ty) {
                "`null` value is unexpected here. Use quotes to treat it as a regular string literal"
            } else {
                "`null` value is unexpected here"
            };
            self.error(node, message);
            return self.error_node(node, contexts);
        }
        if let Yaml::Scalar(text) = self.reader.value(node) {
            if contains_reference(text) {
                let position = self.reader.position(node);
                self.report(position, Severity::Warning, REFERENCE);
            }
        }
        match (ty, self.reader.value(node)) {
            (Type::Object(object), _) => {
                self.refuse_tag(node);
                self.object(node, object, contexts)
            }
            (Type::Dependency(kind), _) => {
                self.refuse_tag(node);
                self.dependency(node, *kind, contexts)
            }
            (Type::List(element), Yaml::Sequence(items)) => {
                self.list(node, items, element, contexts)
            }
            (Type::Map(value), Yaml::Mapping(pairs)) => self.map(node, pairs, value, contexts),
            (Type::Map(value), Yaml::Sequence(items)) => {
                self.map_from_sequence(node, items, value, contexts)
            }
            (scalar, Yaml::Scalar(text)) if !matches!(scalar, Type::List(_) | Type::Map(_)) => {
                self.scalar(node, text, scalar, nullable, contexts)
            }
            _ => {
                self.unexpected(node, &render(ty, nullable, true, true));
                self.error_node(node, contexts)
            }
        }
    }

    /// `reportUnexpectedValue`, with the type already rendered.
    fn unexpected(&mut self, node: NodeId, expected: &str) {
        self.reader.mismatch(node, expected);
    }

    /// `parseScalar`.
    fn scalar(
        &mut self,
        node: NodeId,
        text: &str,
        ty: &Type,
        nullable: bool,
        contexts: Contexts,
    ) -> Node {
        let trace = self.trace(node);
        let value = match ty {
            Type::Boolean => match text {
                "true" => Value::Boolean(true),
                "false" => Value::Boolean(false),
                _ => {
                    self.error(
                        node,
                        format!("Expected: {}", render(ty, nullable, true, false)),
                    );
                    Value::Error
                }
            },
            Type::Int => match parse_int(text) {
                Some(int) => Value::Int(int),
                None => {
                    self.error(
                        node,
                        format!("Expected: {}", render(ty, nullable, true, false)),
                    );
                    Value::Error
                }
            },
            Type::String => Value::String(text.to_string()),
            Type::NonBlankString | Type::MainClass => {
                if text.trim().is_empty() {
                    self.error(node, "Blank value is unexpected here");
                    Value::Error
                } else {
                    Value::String(text.to_string())
                }
            }
            Type::Enum(enumeration) => self.enumeration(node, text, enumeration, None),
            Type::Path => self.path(node, text),
            _ => unreachable!("only scalar types are read from scalars"),
        };
        Node::new(value, trace, contexts)
    }

    /// `parseEnum`; `flag` is a boolean shorthand also offered in the message.
    fn enumeration(
        &mut self,
        node: NodeId,
        text: &str,
        enumeration: &'static EnumType,
        flag: Option<&str>,
    ) -> Value {
        match enumeration.value(text) {
            Some(value) => Value::Enum(enumeration, value),
            None => {
                let suggested: Vec<String> = flag
                    .into_iter()
                    .chain(enumeration.values.iter().copied())
                    .map(|value| format!("`{value}`"))
                    .collect();
                self.error(
                    node,
                    format!(
                        "Unknown value `{text}`. Expected one of: [{}]",
                        suggested.join(", ")
                    ),
                );
                Value::Error
            }
        }
    }

    /// The column of character `offset` of a scalar's value, for a problem inside it.
    fn inside(&self, node: NodeId, offset: usize) -> Position {
        let start = self.reader.position(node);
        let quote = usize::from(!self.is_plain(node));
        Position {
            line: start.line,
            column: start.column + quote + offset,
        }
    }

    /// `parsePath`: relative to the project root (`//`) or to the file's directory.
    fn path(&mut self, node: NodeId, text: &str) -> Value {
        let mut valid = true;
        for (offset, _) in text.char_indices().filter(|(_, c)| *c == '\\') {
            valid = false;
            let position = self.inside(node, text[..offset].chars().count());
            self.report(
                position,
                Severity::Error,
                "Backslash `\\` is not permitted in the path value, use forward slashes `/` instead",
            );
        }
        if !valid {
            return Value::Error;
        }
        let path = if let Some(rooted) = text.strip_prefix("//") {
            let mut offset = 2;
            for segment in rooted.split('/') {
                if segment == "." || segment == ".." {
                    valid = false;
                    let position = self.inside(node, offset);
                    self.report(
                        position,
                        Severity::Error,
                        "`'.'` and `'..'` are not permitted in `//`-paths",
                    );
                }
                offset += segment.chars().count() + 1;
            }
            if !valid {
                return Value::Error;
            }
            self.paths.root.join(rooted)
        } else {
            self.paths.base.join(text)
        };
        Value::Path(normalize(&path))
    }

    fn list(&mut self, node: NodeId, items: &[NodeId], element: &Type, contexts: Contexts) -> Node {
        let children = items
            .iter()
            .map(|&item| self.node(item, element, false, contexts))
            .collect();
        Node::new(Value::List(children), self.trace(node), contexts)
    }

    /// A key of a map, which is always a string (`parseScalarKey`). `None` when it was reported.
    fn map_key(&mut self, key: NodeId) -> Option<String> {
        self.refuse_tag(key);
        let text = self.reader.key(key);
        if text.is_empty() && self.is_plain(key) {
            self.error(key, "Expected a mapping key");
            return None;
        }
        if contains_reference(text) {
            let position = self.reader.position(key);
            self.report(position, Severity::Warning, REFERENCE_KEY);
        }
        Some(text.to_string())
    }

    fn map(
        &mut self,
        node: NodeId,
        pairs: &[(NodeId, NodeId)],
        value: &Type,
        contexts: Contexts,
    ) -> Node {
        let entries = pairs
            .iter()
            .filter_map(|&(key, item)| self.map_entry(key, item, value, contexts))
            .collect();
        Node::new(
            Value::Mapping {
                object: None,
                entries,
            },
            self.trace(node),
            contexts,
        )
    }

    fn map_entry(
        &mut self,
        key: NodeId,
        value: NodeId,
        ty: &Type,
        contexts: Contexts,
    ) -> Option<Entry> {
        let name = self.map_key(key)?;
        Some(Entry {
            key: name,
            property: None,
            key_trace: self.trace(key),
            value: self.node(value, ty, false, contexts),
        })
    }

    /// A map written as a sequence of single-entry mappings.
    fn map_from_sequence(
        &mut self,
        node: NodeId,
        items: &[NodeId],
        value: &Type,
        contexts: Contexts,
    ) -> Node {
        let mut entries = Vec::new();
        for &item in items {
            match self.reader.value(item) {
                Yaml::Mapping([(key, entry)]) => {
                    if let Some(entry) = self.map_entry(*key, *entry, value, contexts) {
                        entries.push(entry);
                    }
                }
                Yaml::Mapping(_) => self.error(item, "Expected a single key-value pair"),
                // The toolchain formats this message without its argument.
                _ => self.error(item, "Expected a key-value pair, but got `{0}`"),
            }
        }
        Node::new(
            Value::Mapping {
                object: None,
                entries,
            },
            self.trace(node),
            contexts,
        )
    }

    /// A property key: its name, with a `test-` prefix and an `@` qualifier read as contexts
    /// (`parsePropertyKeyContexts`). `None` when it was reported.
    fn property_key(&mut self, key: NodeId, contexts: Contexts) -> Option<(String, Contexts)> {
        let text = self.map_key(key)?;
        let (name, qualifier) = match text.find('@') {
            Some(at) => (&text[..at], Some(&text[at + 1..])),
            None => (text.as_str(), None),
        };
        let mut contexts = contexts;
        match qualifier {
            Some(qualifier) if qualifier.contains('+') => {
                self.error(key, "Multiple qualifiers are not supported yet");
                return None;
            }
            Some("jvm") => contexts.jvm = true,
            Some(qualifier) => {
                self.error(
                    key,
                    format!("krusty-toolchain reads JVM modules only; it refuses the qualifier `@{qualifier}` on `{name}`"),
                );
                return None;
            }
            None => {}
        }
        let name = match name {
            "test-settings" => {
                contexts.test = true;
                "settings"
            }
            "test-dependencies" => {
                contexts.test = true;
                "dependencies"
            }
            name => name,
        };
        Some((name.to_string(), contexts))
    }

    /// `parseObject`.
    fn object(&mut self, node: NodeId, object: &'static ObjectType, contexts: Contexts) -> Node {
        if let Some(from_key) = object.key_property() {
            return self.object_from_key(node, object, contexts, |parser, key, contexts| {
                let value = parser.node(key, &from_key.ty, false, contexts);
                Some(vec![Entry {
                    key: from_key.name.to_string(),
                    property: Some(from_key),
                    key_trace: parser.trace(key),
                    value,
                }])
            });
        }
        if object.maven_notation {
            let keyed = match self.reader.value(node) {
                Yaml::Mapping(pairs) => self.notation_of_keys(pairs) == Some(Notation::Maven),
                _ => false,
            };
            if !keyed {
                return self.object_from_key(node, object, contexts, |parser, key, contexts| {
                    match parser.reader.value(key) {
                        Yaml::Scalar(text) => parser.coordinates(key, text, object, contexts),
                        _ => {
                            parser.error(key, "Expected: string");
                            None
                        }
                    }
                });
            }
        }
        self.plain_object(node, object, contexts)
    }

    /// An object whose first property is read from a single key, with the rest nested under it
    /// (`parseObjectWithCustomKeyParsing`).
    fn object_from_key(
        &mut self,
        node: NodeId,
        object: &'static ObjectType,
        contexts: Contexts,
        from_key: impl Fn(&mut Self, NodeId, Contexts) -> Option<Vec<Entry>>,
    ) -> Node {
        let others = object.properties.iter().any(|property| !property.from_key);
        match self.reader.value(node) {
            Yaml::Mapping(pairs) if others => {
                let [(key, rest)] = pairs else {
                    let rendered = render(&Type::Object(object), false, true, false);
                    self.error(
                        node,
                        format!("While parsing `{rendered}`: a mapping must consist of a single key-value pair"),
                    );
                    return self.error_node(node, contexts);
                };
                let Some(mut entries) = from_key(self, *key, contexts) else {
                    return self.error_node(node, contexts);
                };
                let remaining = self.plain_object(*rest, object, contexts);
                let Value::Mapping {
                    entries: rest_entries,
                    ..
                } = remaining.value
                else {
                    return self.error_node(node, contexts);
                };
                entries.extend(rest_entries);
                Node::new(
                    Value::Mapping {
                        object: Some(object),
                        entries,
                    },
                    self.trace(*key),
                    contexts,
                )
            }
            Yaml::Scalar(_) => match from_key(self, node, contexts) {
                Some(entries) => Node::new(
                    Value::Mapping {
                        object: Some(object),
                        entries,
                    },
                    self.trace(node),
                    contexts,
                ),
                None => self.error_node(node, contexts),
            },
            _ => {
                let rendered = render(&Type::Object(object), false, true, false);
                self.unexpected(node, &rendered);
                self.error_node(node, contexts)
            }
        }
    }

    /// `parseObjectWithoutFromKeyProperty`.
    fn plain_object(
        &mut self,
        node: NodeId,
        object: &'static ObjectType,
        contexts: Contexts,
    ) -> Node {
        match self.reader.value(node) {
            Yaml::Mapping(pairs) => {
                let mut entries = Vec::new();
                for &(key, value) in pairs {
                    if let Some(entry) = self.object_entry(key, value, object, contexts) {
                        entries.push(entry);
                    }
                }
                Node::new(
                    Value::Mapping {
                        object: Some(object),
                        entries,
                    },
                    self.trace(node),
                    contexts,
                )
            }
            Yaml::Scalar(text) => self.scalar_shorthand(node, text, object, contexts),
            Yaml::Sequence(items) => match object.value_shorthand() {
                Some(property) if matches!(property.ty, Type::List(_)) => {
                    let Type::List(element) = property.ty else {
                        unreachable!()
                    };
                    let value = self.list(node, items, element, contexts);
                    self.shorthand_object(node, object, property, value, contexts)
                }
                _ => {
                    self.unexpected(node, &render(&Type::Object(object), false, true, true));
                    self.error_node(node, contexts)
                }
            },
            Yaml::Missing | Yaml::Null | Yaml::Absent => {
                self.unexpected(node, &render(&Type::Object(object), false, true, true));
                self.error_node(node, contexts)
            }
        }
    }

    fn object_entry(
        &mut self,
        key: NodeId,
        value: NodeId,
        object: &'static ObjectType,
        contexts: Contexts,
    ) -> Option<Entry> {
        let (name, contexts) = self.property_key(key, contexts)?;
        let property = object.property(&name).filter(|property| !property.from_key);
        let value = match property {
            Some(property) => self.node(value, &property.ty, property.nullable, contexts),
            None => self.undefined(value, contexts),
        };
        Some(Entry {
            key: name,
            property,
            key_trace: self.trace(key),
            value,
        })
    }

    fn shorthand_object(
        &self,
        node: NodeId,
        object: &'static ObjectType,
        property: &'static Property,
        value: Node,
        contexts: Contexts,
    ) -> Node {
        let trace = self.trace(node);
        Node::new(
            Value::Mapping {
                object: Some(object),
                entries: vec![Entry {
                    key: property.name.to_string(),
                    property: Some(property),
                    key_trace: trace.clone(),
                    value,
                }],
            },
            trace,
            contexts,
        )
    }

    /// `parseObjectFromScalarShorthand`.
    fn scalar_shorthand(
        &mut self,
        node: NodeId,
        text: &str,
        object: &'static ObjectType,
        contexts: Contexts,
    ) -> Node {
        let flag = object.flag_shorthand();
        if let Some(flag) = flag.filter(|flag| flag.name == text) {
            let value = Node::new(Value::Boolean(true), self.trace(node), contexts);
            return self.shorthand_object(node, object, flag, value, contexts);
        }
        let secondary = object.value_shorthand();
        let value = match secondary.map(|property| (property, property.ty)) {
            Some((property, Type::Enum(enumeration))) => {
                let value = self.enumeration(node, text, enumeration, flag.map(|flag| flag.name));
                Some((property, Node::new(value, self.trace(node), contexts)))
            }
            Some((property, ty))
                if !matches!(
                    ty,
                    Type::List(_)
                        | Type::Map(_)
                        | Type::Object(_)
                        | Type::Dependency(_)
                        | Type::Opaque
                ) =>
            {
                Some((
                    property,
                    self.scalar(node, text, &ty, property.nullable, contexts),
                ))
            }
            _ => None,
        };
        match value {
            Some((property, value)) => {
                self.shorthand_object(node, object, property, value, contexts)
            }
            None => {
                self.unexpected(node, &render(&Type::Object(object), false, true, true));
                self.error_node(node, contexts)
            }
        }
    }

    /// Maven coordinates `group:artifact[:version[:classifier]][@packaging]` as the coordinate
    /// properties of `object` (`parseMavenCoordinates`). `None` when they were reported.
    fn coordinates(
        &mut self,
        key: NodeId,
        text: &str,
        object: &'static ObjectType,
        contexts: Contexts,
    ) -> Option<Vec<Entry>> {
        if !self.valid_coordinates(key, text) {
            return None;
        }
        let (coordinates, packaging) = match text.split_once('@') {
            Some((coordinates, packaging)) => {
                (coordinates, Some(packaging.split('@').next().unwrap_or("")))
            }
            None => (text, None),
        };
        let parts: Vec<&str> = coordinates.split(':').collect();
        let values = [
            ("groupId", Some(parts[0])),
            ("artifactId", Some(parts[1])),
            ("version", parts.get(2).copied()),
            ("classifier", parts.get(3).copied()),
            ("packagingType", packaging),
        ];
        let trace = self.trace(key);
        Some(
            values
                .into_iter()
                .filter_map(|(name, value)| {
                    Some(Entry {
                        key: name.to_string(),
                        property: object.property(name),
                        key_trace: trace.clone(),
                        value: Node::new(
                            Value::String(value?.to_string()),
                            trace.clone(),
                            contexts,
                        ),
                    })
                })
                .collect(),
        )
    }

    /// `validateAndReportMavenCoordinates`.
    fn valid_coordinates(&mut self, key: NodeId, text: &str) -> bool {
        let refusal = if text.contains(' ') {
            Some("Maven coordinates should not contain spaces".to_string())
        } else if text.contains(['\n', '\r']) {
            Some("Maven coordinates should not contain line breaks".to_string())
        } else if text.contains(['/', '\\']) {
            Some("Maven coordinates should not contain slashes".to_string())
        } else {
            let trimmed = text.trim();
            let mut split = trimmed.split('@');
            let coordinates = split.next().unwrap_or("");
            let packaging = split.next();
            let parts: Vec<&str> = coordinates.split(':').collect();
            if parts.len() < 2 {
                Some(format!(
                    "Maven coordinates `{text}` should contain at least two parts separated by `:`, but got `{}`",
                    parts.len()
                ))
            } else if parts.len() > 4 {
                Some(format!(
                    "Maven coordinates `{text}` should contain at most four parts separated by `:`, but got `{}`",
                    parts.len()
                ))
            } else if parts
                .iter()
                .chain(packaging.iter())
                .any(|part| part.ends_with('.'))
            {
                Some("Maven coordinates should not contain parts ending with dots".to_string())
            } else {
                None
            }
        };
        match refusal {
            Some(message) => {
                self.error(key, message);
                false
            }
            None => true,
        }
    }

    /// The notation a mapping's keys name (`tryInferTypeFromKnownKeys`).
    fn notation_of_keys(&self, pairs: &[(NodeId, NodeId)]) -> Option<Notation> {
        let names: Vec<&str> = pairs
            .iter()
            .map(|&(key, _)| {
                let text = self.reader.key(key);
                text.split('@').next().unwrap_or(text)
            })
            .collect();
        if names
            .iter()
            .any(|name| matches!(*name, "groupId" | "artifactId"))
        {
            Some(Notation::Maven)
        } else if names.contains(&"bom") {
            Some(Notation::Bom)
        } else {
            None
        }
    }

    /// `inferDependencyType`.
    fn notation(&self, node: NodeId, scoped: bool) -> Notation {
        match self.reader.shape(node) {
            Yaml::Mapping(pairs) => {
                if let Some(notation) = self.notation_of_keys(pairs) {
                    return notation;
                }
                match pairs {
                    [(key, _)] if scoped => {
                        // The toolchain reads the key's source text: a quoted key starts with a
                        // quote, and so reads as Maven coordinates.
                        if self.is_plain(*key) {
                            notation_of_text(self.reader.key(*key))
                        } else {
                            Notation::Maven
                        }
                    }
                    _ => Notation::Failed,
                }
            }
            Yaml::Scalar(text) => notation_of_text(text),
            _ => Notation::Failed,
        }
    }

    /// `parseVariant` for the dependency notations.
    fn dependency(&mut self, node: NodeId, kind: DependencyKind, contexts: Contexts) -> Node {
        let notation = self.notation(node, kind == DependencyKind::Module);
        let object = match (kind, notation) {
            (_, Notation::SwiftPackage) => {
                self.error(
                    node,
                    "krusty-toolchain reads JVM modules only; it refuses Swift package dependencies",
                );
                return self.error_node(node, contexts);
            }
            (DependencyKind::Module, Notation::Failed) => {
                self.error(node, WRONG_SCOPED);
                return self.error_node(node, contexts);
            }
            (DependencyKind::Module, Notation::Bom) => &BOM,
            (DependencyKind::Module, Notation::Local) => &INTERNAL,
            (DependencyKind::Module, Notation::Catalog) => &CATALOG,
            (DependencyKind::Module, Notation::Maven) => &EXTERNAL_MAVEN,
            (DependencyKind::Unscoped, Notation::Failed) => {
                self.error(node, WRONG_UNSCOPED);
                return self.error_node(node, contexts);
            }
            (DependencyKind::Unscoped, Notation::Local) => &UNSCOPED_MODULE,
            (DependencyKind::Unscoped, Notation::Bom) => &UNSCOPED_BOM,
            (_, Notation::Catalog) => &UNSCOPED_CATALOG,
            (_, Notation::Maven) => &UNSCOPED_EXTERNAL_MAVEN,
            (DependencyKind::UnscopedExternal, Notation::Local) => {
                self.error(node, "Dependencies on local modules are not supported here");
                return self.error_node(node, contexts);
            }
            (DependencyKind::UnscopedExternal, Notation::Bom) => {
                self.error(node, "BOM dependency is not supported here");
                return self.error_node(node, contexts);
            }
            (DependencyKind::UnscopedExternal, Notation::Failed) => {
                self.error(node, WRONG_UNSCOPED_EXTERNAL);
                return self.error_node(node, contexts);
            }
        };
        self.object(node, object, contexts)
    }

    /// An unknown property's value, read for what it looks like (`parseUndefinedBestEffort`).
    fn undefined(&mut self, node: NodeId, contexts: Contexts) -> Node {
        if matches!(self.reader.value(node), Yaml::Null) {
            return Node::new(Value::Null, self.trace(node), contexts);
        }
        match self.reader.value(node) {
            Yaml::Mapping(pairs) => {
                let mut entries = Vec::new();
                for &(key, value) in pairs {
                    if let Some(name) = self.map_key(key) {
                        let value = self.undefined(value, contexts);
                        entries.push(Entry {
                            key: name,
                            property: None,
                            key_trace: self.trace(key),
                            value,
                        });
                    }
                }
                Node::new(
                    Value::Mapping {
                        object: None,
                        entries,
                    },
                    self.trace(node),
                    contexts,
                )
            }
            Yaml::Scalar(text) => {
                if contains_reference(text) {
                    let position = self.reader.position(node);
                    self.report(position, Severity::Warning, REFERENCE);
                }
                let value = if !self.is_plain(node) {
                    Value::String(text.to_string())
                } else if let Some(int) = simple_int(text) {
                    Value::Int(int)
                } else if text == "true" || text == "false" {
                    Value::Boolean(text == "true")
                } else {
                    Value::String(text.to_string())
                };
                Node::new(value, self.trace(node), contexts)
            }
            Yaml::Sequence(items) => {
                let children = items
                    .iter()
                    .map(|&item| self.undefined(item, contexts))
                    .collect();
                Node::new(Value::List(children), self.trace(node), contexts)
            }
            Yaml::Missing | Yaml::Null | Yaml::Absent => {
                self.unexpected(node, "<undefined-type>");
                self.error_node(node, contexts)
            }
        }
    }
}

/// Kotlin's `String.toIntOrNull`.
fn parse_int(text: &str) -> Option<i32> {
    let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The toolchain's conservative integer pattern for values of no known type: `-?(0|[1-9]\d{0,9})`,
/// read as an `Int`.
fn simple_int(text: &str) -> Option<i32> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    let shaped = digits == "0"
        || (!digits.is_empty()
            && digits.len() <= 10
            && digits.starts_with(|c: char| ('1'..='9').contains(&c))
            && digits.chars().all(|c| c.is_ascii_digit()));
    if shaped {
        text.parse().ok()
    } else {
        None
    }
}

/// `Path.normalize`: `.` dropped and `..` applied lexically.
fn normalize(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normal.pop() {
                    normal.push("..");
                }
            }
            other => normal.push(other),
        }
    }
    normal
}

/// `diagnoseUnknownProperties`: every key an object's type does not have, deepest first.
fn report_unknown_properties(node: &Node, path: &Path, diagnostics: &mut Diagnostics) {
    match &node.value {
        Value::List(children) => {
            for child in children {
                report_unknown_properties(child, path, diagnostics);
            }
        }
        Value::Mapping { object, entries } => {
            for entry in entries {
                report_unknown_properties(&entry.value, path, diagnostics);
            }
            let Some(object) = object else {
                return;
            };
            for entry in entries.iter().filter(|entry| entry.property.is_none()) {
                let meant: Vec<String> = object
                    .properties
                    .iter()
                    .filter(|property| property.misnomers.contains(&entry.key.as_str()))
                    .map(|property| format!("'{}'", property.name))
                    .collect();
                let message = if meant.is_empty() {
                    format!("Unknown property `{}`", entry.key)
                } else {
                    format!(
                        "Unknown property `{}`. Did you mean {}?",
                        entry.key,
                        meant.join(" or ")
                    )
                };
                let position = match entry.key_trace {
                    Trace::File { position, .. } => Some(position),
                    _ => None,
                };
                diagnostics.push(Diagnostic::error(path, position, message));
            }
        }
        _ => {}
    }
}

/// `diagnoseDeprecatedDeclarations`: properties and enum values the schema retired.
fn report_deprecations(node: &Node, path: &Path, diagnostics: &mut Diagnostics) {
    let position = |trace: &Trace| match trace {
        Trace::File { position, .. } => Some(*position),
        _ => None,
    };
    match &node.value {
        Value::List(children) => {
            for child in children {
                report_deprecations(child, path, diagnostics);
            }
        }
        Value::Mapping { entries, .. } => {
            for entry in entries {
                if let Some(deprecation) = entry.property.and_then(|property| property.deprecation)
                {
                    let at = position(&entry.key_trace);
                    diagnostics.push(if deprecation.error {
                        Diagnostic::error(path, at, deprecation.message)
                    } else {
                        Diagnostic::warning(Severity::Warning, path, at, deprecation.message)
                    });
                }
            }
            for entry in entries {
                report_deprecations(&entry.value, path, diagnostics);
            }
        }
        Value::Enum(enumeration, value) => {
            if let Some(message) = enumeration.retirement(value) {
                diagnostics.push(Diagnostic::error(path, position(&node.trace), message));
            }
        }
        _ => {}
    }
}
