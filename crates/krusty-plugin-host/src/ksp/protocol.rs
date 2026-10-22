//! The messages krusty and the KSP host exchange, framed as the plugin host's are
//! (`crate::protocol`). The host's side is `ksp-adapter/krusty/ksphost/Wire.java`; keep the two in
//! step and bump [`PROTOCOL`] on any change.

use std::io::{self, Read, Write};

use krusty_plugin_ir::{
    KspClass, KspClassKind, KspRound, KspType, KspTypeArgument, KspTypeDeclaration,
    KspTypeParameter, KspVariance,
};

use crate::protocol::{malformed, put_list, put_str, read_frame, utf8, write_frame, Payload};

use super::{GeneratedKind, KspGenerated, KspLoad, KspOutcome};

pub(crate) const PROTOCOL: u32 = 2;

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

pub(crate) enum Request<'a> {
    Load(&'a KspLoad<'a>),
    Round(&'a KspRound),
    Finish,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Reply {
    /// The processor providers found on the classpath, in service-loader order.
    Loaded(Vec<String>),
    RoundDone(KspOutcome),
    Finished(KspOutcome),
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
                put_list(payload, &file.classes, put_class);
            });
        }
        Request::Finish => payload.push(FINISH),
    }
    write_frame(out, &payload)
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
    put_list(payload, &class.annotations, |payload, annotation| {
        put_str(payload, &annotation.short_name);
        put_str(payload, &annotation.qualified_name);
    });
    put_option(payload, &class.modifiers, |payload, modifiers| {
        put_list(payload, modifiers, |payload, modifier| {
            put_str(payload, modifier.name());
        });
    });
    put_option(payload, &class.type_parameters, |payload, parameters| {
        put_list(payload, parameters, put_type_parameter);
    });
    put_option(payload, &class.super_types, |payload, super_types| {
        put_list(payload, super_types, put_type);
    });
    put_option(
        payload,
        &class.primary_constructor,
        |payload, parameters| {
            put_list(payload, parameters, |payload, parameter| {
                put_str(payload, &parameter.name);
                payload.push(
                    u8::from(parameter.is_val)
                        | u8::from(parameter.is_var) << 1
                        | u8::from(parameter.is_vararg) << 2
                        | u8::from(parameter.has_default) << 3,
                );
            });
        },
    );
    payload.push(u8::from(class.declarations_complete));
    put_list(payload, &class.classes, put_class);
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
    put_str(payload, &parameter.name);
    payload.push(match parameter.variance {
        KspVariance::Invariant => 0,
        KspVariance::Out => 1,
        KspVariance::In => 2,
    });
    payload.push(u8::from(parameter.is_reified));
    put_list(payload, &parameter.bounds, put_type);
}

/// A type: its declaration (0 and a classifier's qualified name, or 1 and a type parameter's
/// owner and name), whether it is nullable, then its arguments (0 for a star, else 1 invariant,
/// 2 `out` or 3 `in`, and the type).
fn put_type(payload: &mut Vec<u8>, ty: &KspType) {
    match &ty.declaration {
        KspTypeDeclaration::Class(name) => {
            payload.push(0);
            put_str(payload, name);
        }
        KspTypeDeclaration::TypeParameter { owner, name } => {
            payload.push(1);
            put_str(payload, owner);
            put_str(payload, name);
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
    use krusty_plugin_ir::{KspAnnotation, KspFile, KspModifier, KspParameter};

    #[test]
    fn a_round_frames_each_class_with_its_nested_classes_after_its_own_facts() {
        let round = KspRound {
            files: vec![KspFile {
                path: "/src/A.kt".into(),
                package_name: "p".into(),
                classes: vec![KspClass {
                    simple_name: "A".into(),
                    qualified_name: "p.A".into(),
                    kind: KspClassKind::Object,
                    line: 3,
                    annotations: vec![KspAnnotation {
                        short_name: "M".into(),
                        qualified_name: "p.M".into(),
                    }],
                    modifiers: Some(vec![KspModifier::Data]),
                    type_parameters: Some(vec![KspTypeParameter {
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
                        arguments: vec![
                            KspTypeArgument::Star,
                            KspTypeArgument::In(KspType {
                                declaration: KspTypeDeclaration::TypeParameter {
                                    owner: "p.A".into(),
                                    name: "T".into(),
                                },
                                nullable: false,
                                arguments: Vec::new(),
                            }),
                        ],
                    }]),
                    primary_constructor: Some(vec![KspParameter {
                        name: "x".into(),
                        is_val: true,
                        is_var: false,
                        is_vararg: false,
                        has_default: true,
                    }]),
                    classes: Vec::new(),
                    declarations_complete: true,
                }],
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
        expected.extend([0, 0, 0, 1]);
        expected.extend([0, 0, 0, 1, b'A', 0, 0, 0, 3]);
        expected.extend(b"p.A");
        expected.extend([4, 0, 0, 0, 3]);
        expected.extend([0, 0, 0, 1, 0, 0, 0, 1, b'M', 0, 0, 0, 3]);
        expected.extend(b"p.M");
        // Written `data`.
        expected.extend([1, 0, 0, 0, 1, 0, 0, 0, 4]);
        expected.extend(b"DATA");
        // `out T : k.Any?`.
        expected.extend([
            1, 0, 0, 0, 1, 0, 0, 0, 1, b'T', 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 5,
        ]);
        expected.extend(b"k.Any");
        expected.extend([1, 0, 0, 0, 0]);
        // `k.C<*, in T>`.
        expected.extend([1, 0, 0, 0, 1, 0, 0, 0, 0, 3]);
        expected.extend(b"k.C");
        expected.extend([0, 0, 0, 0, 2, 0, 3, 1, 0, 0, 0, 3]);
        expected.extend(b"p.A");
        expected.extend([0, 0, 0, 1, b'T', 0, 0, 0, 0, 0]);
        // A primary constructor with one `val x = …` parameter, then the complete declarations.
        expected.extend([1, 0, 0, 0, 1, 0, 0, 0, 1, b'x', 0b1001]);
        expected.extend([1, 0, 0, 0, 0]);
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
}
