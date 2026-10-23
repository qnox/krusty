//! The messages krusty and the KSP host exchange, framed as the plugin host's are
//! (`crate::protocol`). The host's side is `ksp-adapter/krusty/ksphost/Wire.java`; keep the two in
//! step and bump [`PROTOCOL`] on any change.

use std::io::{self, Read, Write};

use krusty_plugin_ir::{
    KspAccessor, KspAnnotation, KspArgument, KspClass, KspClassKind, KspDeclaration, KspFunction,
    KspFunctionKind, KspLibraryClass, KspModifier, KspNameMapping, KspOrigin, KspParameter,
    KspProperty, KspRound, KspType, KspTypeArgument, KspTypeDeclaration, KspTypeParameter,
    KspTypeSlot, KspValue, KspVariance,
};

use crate::protocol::{malformed, put_list, put_str, read_frame, utf8, write_frame, Payload};

use super::{GeneratedKind, KspGenerated, KspLoad, KspOutcome};

pub(crate) const PROTOCOL: u32 = 6;

const HELLO: u8 = 0;
const LOAD: u8 = 1;
const LOADED: u8 = 2;
const FAILED: u8 = 3;
const UNSUPPORTED: u8 = 4;
const ROUND: u8 = 5;
const ROUND_DONE: u8 = 6;
const FINISH: u8 = 7;
const FINISHED: u8 = 8;
pub(crate) const SHUTDOWN: u8 = 9;
const CLASS_QUERY: u8 = 10;
const CLASS_ANSWER: u8 = 11;
const NAME_QUERY: u8 = 12;
const NAME_ANSWER: u8 = 13;

pub(crate) enum Request<'a> {
    Load(&'a KspLoad<'a>),
    Round(&'a KspRound),
    Finish,
    /// The answer to a [`Reply::ClassQuery`]: the classifier, or `None` when the classpath has
    /// none by that name.
    ClassAnswer(Option<&'a KspLibraryClass>),
    /// The answer to a [`Reply::NameQuery`]: the mapped name, or `None` when there is none.
    NameAnswer(Option<&'a str>),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Reply {
    /// The processor providers found on the classpath, in service-loader order.
    Loaded(Vec<String>),
    RoundDone(KspOutcome),
    Finished(KspOutcome),
    /// Not a reply but a question in the middle of a request: a processor reached the classpath
    /// classifier with this qualified name. The host waits for its [`Request::ClassAnswer`].
    ClassQuery(String),
    /// Likewise a question: a processor asked for the name this mapping gives a qualified name.
    /// The host waits for its [`Request::NameAnswer`].
    NameQuery(KspNameMapping, String),
    /// A processor reached an API member the host does not implement yet.
    Unsupported(String),
    /// The request failed in the host, with the stack trace that says why.
    Failed(String),
}

pub(crate) fn write_request(out: &mut impl Write, request: &Request) -> io::Result<()> {
    let mut payload = Vec::new();
    match request {
        Request::Load(load) => {
            payload.push(LOAD);
            let classpath = load
                .processor_classpath
                .iter()
                .map(|entry| utf8(entry))
                .collect::<io::Result<Vec<_>>>()?;
            put_list(&mut payload, &classpath, |payload, entry| {
                put_str(payload, entry)
            });
            put_list(&mut payload, load.options, |payload, (key, value)| {
                put_str(payload, key);
                put_str(payload, value);
            });
            put_str(&mut payload, load.module_name);
            for dir in [
                &load.outputs.kotlin,
                &load.outputs.java,
                &load.outputs.classes,
                &load.outputs.resources,
            ] {
                put_str(&mut payload, utf8(dir)?);
            }
            for value in [
                load.language_version,
                load.api_version,
                load.compiler_version,
                load.ksp_version,
                load.jvm_target,
                load.jvm_default_mode,
            ] {
                put_str(&mut payload, value);
            }
            payload.extend_from_slice(&load.log_level.to_be_bytes());
            payload.push(u8::from(load.warnings_as_errors));
        }
        Request::Round(round) => {
            payload.push(ROUND);
            put_list(&mut payload, &round.files, |payload, file| {
                put_str(payload, &file.path);
                put_str(payload, &file.package_name);
                payload.push(u8::from(file.declarations_complete));
                put_list(payload, &file.declarations, put_declaration);
            });
        }
        Request::Finish => payload.push(FINISH),
        Request::ClassAnswer(answer) => {
            payload.push(CLASS_ANSWER);
            put_option(&mut payload, answer, |payload, library| {
                put_library_class(payload, library)
            });
        }
        Request::NameAnswer(answer) => {
            payload.push(NAME_ANSWER);
            put_option(&mut payload, answer, |payload, name| put_str(payload, name));
        }
    }
    write_frame(out, &payload)
}

/// A declaration: 0 and a classifier, 1 and a function, 2 and a property, or 3 and a type alias.
fn put_declaration(payload: &mut Vec<u8>, declaration: &KspDeclaration) {
    match declaration {
        KspDeclaration::Class(class) => {
            payload.push(0);
            put_class(payload, class);
        }
        KspDeclaration::Function(function) => {
            payload.push(1);
            put_function(payload, function);
        }
        KspDeclaration::Property(property) => {
            payload.push(2);
            put_property(payload, property);
        }
        KspDeclaration::TypeAlias(alias) => {
            payload.push(3);
            put_str(payload, &alias.simple_name);
            put_str(payload, &alias.qualified_name);
        }
    }
}

fn put_class(payload: &mut Vec<u8>, class: &KspClass) {
    put_str(payload, &class.simple_name);
    put_str(payload, &class.qualified_name);
    payload.push(match class.kind {
        KspClassKind::Interface => 0,
        KspClassKind::Class => 1,
        KspClassKind::EnumClass => 2,
        KspClassKind::EnumEntry => 3,
        KspClassKind::Object => 4,
        KspClassKind::AnnotationClass => 5,
    });
    payload.extend_from_slice(&class.line.to_be_bytes());
    put_annotations(payload, &class.annotations);
    put_modifiers(payload, &class.modifiers);
    put_type_parameters(payload, &class.type_parameters);
    put_option(payload, &class.super_types, |payload, super_types| {
        put_list(payload, super_types, put_type);
    });
    payload.push(u8::from(class.declarations_complete));
    put_list(payload, &class.declarations, put_declaration);
}

/// A classpath classifier: its header as a class's, then its package, the qualified name of the
/// classifier it is nested in if any, its origin (0 Kotlin, 1 Java), and whether its annotations
/// are complete.
fn put_library_class(payload: &mut Vec<u8>, library: &KspLibraryClass) {
    put_class(payload, &library.class);
    put_str(payload, &library.package_name);
    put_option(payload, &library.parent, |payload, parent| {
        put_str(payload, parent)
    });
    payload.push(match library.origin {
        KspOrigin::KotlinLib => 0,
        KspOrigin::JavaLib => 1,
    });
    payload.push(u8::from(library.annotations_complete));
}

/// A function: its name, kind (0 top-level, 1 member, 2 static, 3 primary constructor,
/// 4 secondary constructor), whether it is abstract, its line, annotations, modifiers, type
/// parameters, extension receiver, parameters and return type.
fn put_function(payload: &mut Vec<u8>, function: &KspFunction) {
    put_str(payload, &function.name);
    payload.push(match function.kind {
        KspFunctionKind::TopLevel => 0,
        KspFunctionKind::Member => 1,
        KspFunctionKind::Static => 2,
        KspFunctionKind::PrimaryConstructor => 3,
        KspFunctionKind::SecondaryConstructor => 4,
    });
    payload.push(u8::from(function.is_abstract));
    put_option(payload, &function.line, |payload, line| {
        payload.extend_from_slice(&line.to_be_bytes());
    });
    put_annotations(payload, &function.annotations);
    put_modifiers(payload, &function.modifiers);
    put_type_parameters(payload, &function.type_parameters);
    put_type_slot(payload, &function.extension_receiver);
    put_list(payload, &function.parameters, put_parameter);
    put_option(payload, &function.return_type, put_type);
}

fn put_property(payload: &mut Vec<u8>, property: &KspProperty) {
    put_str(payload, &property.name);
    payload.push(u8::from(property.is_mutable));
    put_option(payload, &property.line, |payload, line| {
        payload.extend_from_slice(&line.to_be_bytes());
    });
    put_annotations(payload, &property.annotations);
    put_modifiers(payload, &property.modifiers);
    put_type_parameters(payload, &property.type_parameters);
    put_type_slot(payload, &property.extension_receiver);
    put_option(payload, &property.ty, put_type);
    put_accessor(payload, &property.getter);
    put_option(payload, &property.setter, put_accessor);
}

/// An accessor: whether the source wrote it, its modifiers and annotations, and a setter's
/// parameter name and annotations.
fn put_accessor(payload: &mut Vec<u8>, accessor: &KspAccessor) {
    payload.push(u8::from(accessor.written));
    put_modifiers(payload, &accessor.modifiers);
    put_annotations(payload, &accessor.annotations);
    put_option(
        payload,
        &accessor.parameter,
        |payload, (name, annotations)| {
            put_str(payload, name);
            put_annotations(payload, annotations);
        },
    );
}

/// A parameter: its name, its type and a flags byte (bit 0 `val`, 1 `var`, 2 vararg, 3 default,
/// 4 `noinline`, 5 `crossinline`, 6 whether it has a default is not described).
fn put_parameter(payload: &mut Vec<u8>, parameter: &KspParameter) {
    put_str(payload, &parameter.name);
    put_option(payload, &parameter.ty, put_type);
    payload.push(
        u8::from(parameter.is_val)
            | u8::from(parameter.is_var) << 1
            | u8::from(parameter.is_vararg) << 2
            | u8::from(parameter.has_default == Some(true)) << 3
            | u8::from(parameter.is_noinline) << 4
            | u8::from(parameter.is_crossinline) << 5
            | u8::from(parameter.has_default.is_none()) << 6,
    );
    put_annotations(payload, &parameter.annotations);
}

fn put_annotations(payload: &mut Vec<u8>, annotations: &[KspAnnotation]) {
    put_list(payload, annotations, put_annotation);
}

/// An annotation: its short and qualified names, its use-site target's name (empty for none),
/// whether it is synthetic, then its arguments and defaults when the round folded them.
fn put_annotation(payload: &mut Vec<u8>, annotation: &KspAnnotation) {
    put_str(payload, &annotation.short_name);
    put_str(payload, &annotation.qualified_name);
    put_str(payload, annotation.use_site.map_or("", |site| site.name()));
    payload.push(u8::from(annotation.synthetic));
    put_option(payload, &annotation.arguments, |payload, arguments| {
        put_list(payload, &arguments.arguments, put_argument);
        put_list(payload, &arguments.defaults, put_argument);
    });
}

fn put_argument(payload: &mut Vec<u8>, argument: &KspArgument) {
    put_str(payload, &argument.name);
    payload.push(u8::from(argument.written));
    put_value(payload, &argument.value);
}

/// A value: a tag byte (0 boolean, 1 byte, 2 short, 3 int, 4 long, 5 char, 6 float bits,
/// 7 double bits, 8 string, 9 enum entry, 10 type, 11 annotation, 12 array), then the value.
fn put_value(payload: &mut Vec<u8>, value: &KspValue) {
    match value {
        KspValue::Boolean(value) => {
            payload.push(0);
            payload.push(u8::from(*value));
        }
        KspValue::Byte(value) => {
            payload.push(1);
            payload.extend_from_slice(&value.to_be_bytes());
        }
        KspValue::Short(value) => {
            payload.push(2);
            payload.extend_from_slice(&value.to_be_bytes());
        }
        KspValue::Int(value) => {
            payload.push(3);
            payload.extend_from_slice(&value.to_be_bytes());
        }
        KspValue::Long(value) => {
            payload.push(4);
            payload.extend_from_slice(&value.to_be_bytes());
        }
        KspValue::Char(value) => {
            payload.push(5);
            payload.extend_from_slice(&value.to_be_bytes());
        }
        KspValue::Float(bits) => {
            payload.push(6);
            payload.extend_from_slice(&bits.to_be_bytes());
        }
        KspValue::Double(bits) => {
            payload.push(7);
            payload.extend_from_slice(&bits.to_be_bytes());
        }
        KspValue::String(value) => {
            payload.push(8);
            put_str(payload, value);
        }
        KspValue::EnumEntry { class, entry } => {
            payload.push(9);
            put_str(payload, class);
            put_str(payload, entry);
        }
        KspValue::Type(ty) => {
            payload.push(10);
            put_type(payload, ty);
        }
        KspValue::Annotation(annotation) => {
            payload.push(11);
            put_annotation(payload, annotation);
        }
        KspValue::Array(values) => {
            payload.push(12);
            put_list(payload, values, put_value);
        }
    }
}

fn put_modifiers(payload: &mut Vec<u8>, modifiers: &Option<Vec<KspModifier>>) {
    put_option(payload, modifiers, |payload, modifiers| {
        put_list(payload, modifiers, |payload, modifier| {
            put_str(payload, modifier.name());
        });
    });
}

fn put_type_parameters(payload: &mut Vec<u8>, parameters: &Option<Vec<KspTypeParameter>>) {
    put_option(payload, parameters, |payload, parameters| {
        put_list(payload, parameters, put_type_parameter);
    });
}

/// 0 when absent, 1 and the type when present, 2 when present but undescribed.
fn put_type_slot(payload: &mut Vec<u8>, slot: &KspTypeSlot) {
    match slot {
        KspTypeSlot::Absent => payload.push(0),
        KspTypeSlot::Present(ty) => {
            payload.push(1);
            put_type(payload, ty);
        }
        KspTypeSlot::Undescribed => payload.push(2),
    }
}

/// A presence byte, then the value when there is one.
fn put_option<T>(payload: &mut Vec<u8>, value: &Option<T>, put: impl FnOnce(&mut Vec<u8>, &T)) {
    match value {
        Some(value) => {
            payload.push(1);
            put(payload, value);
        }
        None => payload.push(0),
    }
}

fn put_type_parameter(payload: &mut Vec<u8>, parameter: &KspTypeParameter) {
    payload.extend_from_slice(&parameter.id.to_be_bytes());
    put_str(payload, &parameter.name);
    payload.push(match parameter.variance {
        KspVariance::Invariant => 0,
        KspVariance::Out => 1,
        KspVariance::In => 2,
    });
    payload.push(u8::from(parameter.is_reified));
    put_list(payload, &parameter.bounds, put_type);
}

/// A type: its declaration (0 and a classifier's qualified name, 1 and a type parameter's id,
/// owner and name, or 2 and a type alias's qualified and package names), whether it is nullable, then its arguments (0 for a star, else 1 invariant,
/// 2 `out` or 3 `in`, and the type).
fn put_type(payload: &mut Vec<u8>, ty: &KspType) {
    match &ty.declaration {
        KspTypeDeclaration::Class(name) => {
            payload.push(0);
            put_str(payload, name);
        }
        KspTypeDeclaration::TypeParameter { id, owner, name } => {
            payload.push(1);
            payload.extend_from_slice(&id.to_be_bytes());
            put_str(payload, owner);
            put_str(payload, name);
        }
        KspTypeDeclaration::TypeAlias {
            qualified_name,
            package_name,
        } => {
            payload.push(2);
            put_str(payload, qualified_name);
            put_str(payload, package_name);
        }
    }
    payload.push(u8::from(ty.nullable));
    put_list(payload, &ty.arguments, |payload, argument| match argument {
        KspTypeArgument::Star => payload.push(0),
        KspTypeArgument::Invariant(ty) => {
            payload.push(1);
            put_type(payload, ty);
        }
        KspTypeArgument::Out(ty) => {
            payload.push(2);
            put_type(payload, ty);
        }
        KspTypeArgument::In(ty) => {
            payload.push(3);
            put_type(payload, ty);
        }
    });
}

/// The protocol version of the host's greeting.
pub(crate) fn read_hello(input: &mut impl Read) -> io::Result<u32> {
    let frame = read_frame(input)?;
    let mut payload = Payload(&frame);
    match payload.byte()? {
        HELLO => payload.u32(),
        tag => Err(malformed(format!(
            "expected the KSP host's greeting, got message {tag}"
        ))),
    }
}

pub(crate) fn read_reply(input: &mut impl Read) -> io::Result<Reply> {
    let frame = read_frame(input)?;
    let mut payload = Payload(&frame);
    match payload.byte()? {
        LOADED => Ok(Reply::Loaded(payload.list(|payload| payload.string())?)),
        ROUND_DONE => Ok(Reply::RoundDone(outcome(&mut payload)?)),
        FINISHED => Ok(Reply::Finished(outcome(&mut payload)?)),
        CLASS_QUERY => Ok(Reply::ClassQuery(payload.string()?)),
        NAME_QUERY => {
            let mapping = match payload.byte()? {
                0 => KspNameMapping::KotlinToJava,
                1 => KspNameMapping::JavaToKotlin,
                tag => return Err(malformed(format!("unknown name mapping {tag}"))),
            };
            Ok(Reply::NameQuery(mapping, payload.string()?))
        }
        UNSUPPORTED => Ok(Reply::Unsupported(payload.string()?)),
        FAILED => Ok(Reply::Failed(payload.string()?)),
        tag => Err(malformed(format!("unknown reply {tag}"))),
    }
}

fn outcome(payload: &mut Payload) -> io::Result<KspOutcome> {
    let generated = payload.list(|payload| {
        let kind = match payload.byte()? {
            0 => GeneratedKind::Kotlin,
            1 => GeneratedKind::Java,
            2 => GeneratedKind::Class,
            3 => GeneratedKind::Resource,
            tag => return Err(malformed(format!("unknown generated file kind {tag}"))),
        };
        Ok(KspGenerated {
            kind,
            path: payload.string()?.into(),
        })
    })?;
    let log = payload.list(|payload| payload.string())?;
    let has_error = payload.byte()? != 0;
    Ok(KspOutcome {
        generated,
        log,
        has_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use krusty_plugin_ir::KspFile;

    fn parameter_t() -> KspType {
        KspType {
            declaration: KspTypeDeclaration::TypeParameter {
                id: 7,
                owner: "p.A".into(),
                name: "T".into(),
            },
            nullable: false,
            arguments: Vec::new(),
        }
    }

    #[test]
    fn a_round_frames_each_class_with_its_declarations_after_its_own_facts() {
        let constructor = KspFunction {
            name: "<init>".into(),
            kind: KspFunctionKind::PrimaryConstructor,
            is_abstract: false,
            line: None,
            annotations: Vec::new(),
            modifiers: Some(vec![KspModifier::Final]),
            type_parameters: None,
            extension_receiver: KspTypeSlot::Absent,
            parameters: vec![KspParameter {
                name: "x".into(),
                ty: Some(parameter_t()),
                is_val: true,
                is_var: false,
                is_vararg: false,
                is_noinline: false,
                is_crossinline: false,
                has_default: Some(true),
                annotations: Vec::new(),
            }],
            return_type: None,
        };
        let round = KspRound {
            files: vec![KspFile {
                path: "/src/A.kt".into(),
                package_name: "p".into(),
                declarations: vec![KspDeclaration::Class(KspClass {
                    simple_name: "A".into(),
                    qualified_name: "p.A".into(),
                    kind: KspClassKind::Object,
                    line: 3,
                    annotations: vec![KspAnnotation {
                        short_name: "M".into(),
                        qualified_name: "p.M".into(),
                        use_site: None,
                        synthetic: false,
                        arguments: Some(krusty_plugin_ir::KspAnnotationArguments {
                            arguments: vec![KspArgument {
                                name: "v".into(),
                                value: KspValue::Int(5),
                                written: true,
                            }],
                            defaults: Vec::new(),
                        }),
                    }],
                    modifiers: Some(vec![KspModifier::Data]),
                    type_parameters: Some(vec![KspTypeParameter {
                        id: 7,
                        name: "T".into(),
                        variance: KspVariance::Out,
                        is_reified: false,
                        bounds: vec![KspType {
                            declaration: KspTypeDeclaration::Class("k.Any".into()),
                            nullable: true,
                            arguments: Vec::new(),
                        }],
                    }]),
                    super_types: Some(vec![KspType {
                        declaration: KspTypeDeclaration::Class("k.C".into()),
                        nullable: false,
                        arguments: vec![KspTypeArgument::Star, KspTypeArgument::In(parameter_t())],
                    }]),
                    declarations: vec![KspDeclaration::Function(constructor)],
                    declarations_complete: true,
                })],
                declarations_complete: false,
            }],
        };
        let mut framed = Vec::new();
        write_request(&mut framed, &Request::Round(&round)).unwrap();
        let mut expected = vec![ROUND];
        expected.extend([0, 0, 0, 1]);
        expected.extend([0, 0, 0, 9]);
        expected.extend(b"/src/A.kt");
        expected.extend([0, 0, 0, 1, b'p', 0]);
        // One declaration, a classifier.
        expected.extend([0, 0, 0, 1, 0]);
        expected.extend([0, 0, 0, 1, b'A', 0, 0, 0, 3]);
        expected.extend(b"p.A");
        expected.extend([4, 0, 0, 0, 3]);
        expected.extend([0, 0, 0, 1, 0, 0, 0, 1, b'M', 0, 0, 0, 3]);
        expected.extend(b"p.M");
        // No use-site target, not synthetic, and the written argument `v = 5` with no defaults.
        expected.extend([
            0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, b'v', 1, 3, 0, 0, 0, 5,
        ]);
        expected.extend([0, 0, 0, 0]);
        // Written `data`.
        expected.extend([1, 0, 0, 0, 1, 0, 0, 0, 4]);
        expected.extend(b"DATA");
        // `out T : k.Any?`, type parameter 7.
        expected.extend([1, 0, 0, 0, 1, 0, 0, 0, 7, 0, 0, 0, 1, b'T', 1, 0]);
        expected.extend([0, 0, 0, 1, 0, 0, 0, 0, 5]);
        expected.extend(b"k.Any");
        expected.extend([1, 0, 0, 0, 0]);
        // `k.C<*, in T>`.
        expected.extend([1, 0, 0, 0, 1, 0, 0, 0, 0, 3]);
        expected.extend(b"k.C");
        expected.extend([0, 0, 0, 0, 2, 0, 3]);
        let t = {
            let mut t = vec![1, 0, 0, 0, 7, 0, 0, 0, 3];
            t.extend(b"p.A");
            t.extend([0, 0, 0, 1, b'T', 0, 0, 0, 0, 0]);
            t
        };
        expected.extend(&t);
        // The complete declarations: a primary constructor, final, with one `val x: T = …`.
        expected.extend([1, 0, 0, 0, 1, 1, 0, 0, 0, 6]);
        expected.extend(b"<init>");
        expected.extend([3, 0, 0, 0, 0, 0, 0]);
        expected.extend([1, 0, 0, 0, 1, 0, 0, 0, 5]);
        expected.extend(b"FINAL");
        expected.extend([0, 0, 0, 0, 0, 1, 0, 0, 0, 1, b'x', 1]);
        expected.extend(&t);
        // Its flags, no annotations, and no return type.
        expected.extend([0b1001, 0, 0, 0, 0, 0]);
        let mut frame = (expected.len() as u32).to_be_bytes().to_vec();
        frame.extend(expected);
        assert_eq!(framed, frame);
    }

    #[test]
    fn a_round_reply_reads_back_every_generated_file_and_log_line() {
        let mut payload = vec![ROUND_DONE, 0, 0, 0, 2, 0];
        payload.extend([0, 0, 0, 4]);
        payload.extend(b"/k/A");
        payload.push(3);
        payload.extend([0, 0, 0, 4]);
        payload.extend(b"/r/B");
        payload.extend([0, 0, 0, 1, 0, 0, 0, 3]);
        payload.extend(b"w: ");
        payload.push(1);
        let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
        frame.extend(payload);
        assert_eq!(
            read_reply(&mut frame.as_slice()).unwrap(),
            Reply::RoundDone(KspOutcome {
                generated: vec![
                    KspGenerated {
                        kind: GeneratedKind::Kotlin,
                        path: "/k/A".into()
                    },
                    KspGenerated {
                        kind: GeneratedKind::Resource,
                        path: "/r/B".into()
                    },
                ],
                log: vec!["w: ".into()],
                has_error: true,
            })
        );
    }

    #[test]
    fn a_name_query_reads_back_its_mapping_and_its_answer_frames_the_name() {
        let frame = |payload: Vec<u8>| {
            let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
            frame.extend(payload);
            frame
        };
        let query = |direction: u8| {
            let mut payload = vec![NAME_QUERY, direction, 0, 0, 0, 10];
            payload.extend(b"kotlin.Int");
            frame(payload)
        };
        assert_eq!(
            read_reply(&mut query(0).as_slice()).unwrap(),
            Reply::NameQuery(KspNameMapping::KotlinToJava, "kotlin.Int".into())
        );
        assert_eq!(
            read_reply(&mut query(1).as_slice()).unwrap(),
            Reply::NameQuery(KspNameMapping::JavaToKotlin, "kotlin.Int".into())
        );
        assert_eq!(
            read_reply(&mut query(2).as_slice())
                .unwrap_err()
                .to_string(),
            "unknown name mapping 2"
        );
        let mut framed = Vec::new();
        write_request(&mut framed, &Request::NameAnswer(Some("java.lang.Integer"))).unwrap();
        let mut answer = vec![NAME_ANSWER, 1, 0, 0, 0, 17];
        answer.extend(b"java.lang.Integer");
        assert_eq!(framed, frame(answer));
        let mut framed = Vec::new();
        write_request(&mut framed, &Request::NameAnswer(None)).unwrap();
        assert_eq!(framed, frame(vec![NAME_ANSWER, 0]));
    }
}
