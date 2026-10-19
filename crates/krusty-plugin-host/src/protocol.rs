//! The messages krusty and the host exchange over the host's stdin and stdout.
//!
//! Each message is one frame: a big-endian `u32` payload length, then the payload, whose first byte
//! is the message's tag. Strings are a `u32` byte length and UTF-8; lists are a `u32` count and the
//! elements. The host's side is `adapter/`; keep the two in step and bump [`PROTOCOL`] on any
//! change.

use std::io::{self, Read, Write};

use crate::session::{
    CallableIdentity, CallableKind, ClassName, Classifier, SessionRequest, TypeArgument,
    TypeIdentity,
};

pub(crate) const PROTOCOL: u32 = 2;

const HELLO: u8 = 0;
const LOAD: u8 = 1;
const LOADED: u8 = 2;
const FAILED: u8 = 3;
const SHUTDOWN: u8 = 4;
const UNLOAD: u8 = 5;
const UNLOADED: u8 = 6;
const OPEN_SESSION: u8 = 7;
const SESSION_OPENED: u8 = 8;
const CLOSE_SESSION: u8 = 9;
const SESSION_CLOSED: u8 = 10;
const RESOLVE: u8 = 11;
const RESOLVED: u8 = 12;

/// A frame larger than this is a broken stream, not a message.
const MAX_FRAME: u32 = 1 << 30;

/// What the host sends first, before any request.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Hello {
    pub protocol: u32,
    /// `KotlinCompilerVersion.VERSION` of the compiler the host loaded.
    pub kotlin_version: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Request<'a> {
    /// Load a plugin classpath with its `-P` option values, replacing what was loaded.
    Load {
        entries: &'a [String],
        options: &'a [String],
    },
    /// End the loaded plugins' lifetime: dispose their resources and close their class loader.
    Unload,
    /// Open a compilation's session, replacing the one open.
    OpenSession(&'a SessionRequest),
    CloseSession,
    /// Find declarations on the session's classpath.
    Resolve(&'a [CallableIdentity]),
    Shutdown,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Reply {
    Loaded(Vec<LoadedPlugin>),
    Unloaded,
    SessionOpened,
    SessionClosed,
    /// Each resolved declaration rendered as kotlinc renders IR, in request order.
    Resolved(Vec<String>),
    /// The request failed in the host; the message says why, as kotlinc would.
    Failed(String),
}

/// One registrar a plugin classpath declared, after it registered its extensions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedPlugin {
    /// The registrar's class.
    pub registrar: String,
    pub plugin_id: String,
    pub supports_k2: bool,
    /// Each extension point the plugin registered extensions for, by kotlinc's name for it, with how
    /// many, in registration order.
    pub extensions: Vec<(String, u32)>,
}

pub(crate) fn write_request(out: &mut impl Write, request: &Request) -> io::Result<()> {
    let mut payload = Vec::new();
    match request {
        Request::Load { entries, options } => {
            payload.push(LOAD);
            put_list(&mut payload, entries, |payload, entry| {
                put_str(payload, entry)
            });
            put_list(&mut payload, options, |payload, option| {
                put_str(payload, option)
            });
        }
        Request::Unload => payload.push(UNLOAD),
        Request::OpenSession(session) => {
            payload.push(OPEN_SESSION);
            let classpath = session
                .classpath
                .iter()
                .map(|entry| utf8(entry))
                .collect::<io::Result<Vec<_>>>()?;
            put_list(&mut payload, &classpath, |payload, entry| {
                put_str(payload, entry)
            });
            put_str(&mut payload, utf8(&session.jdk_home)?);
            put_str(&mut payload, &session.module_name);
            put_str(&mut payload, &session.language_version);
            put_str(&mut payload, &session.api_version);
            put_str(&mut payload, &session.jvm_target);
        }
        Request::CloseSession => payload.push(CLOSE_SESSION),
        Request::Resolve(identities) => {
            payload.push(RESOLVE);
            put_list(&mut payload, identities, put_callable);
        }
        Request::Shutdown => payload.push(SHUTDOWN),
    }
    write_frame(out, &payload)
}

pub(crate) fn read_hello(input: &mut impl Read) -> io::Result<Hello> {
    let frame = read_frame(input)?;
    let mut payload = Payload(&frame);
    match payload.byte()? {
        HELLO => Ok(Hello {
            protocol: payload.u32()?,
            kotlin_version: payload.string()?,
        }),
        tag => Err(malformed(format!(
            "expected the host's greeting, got message {tag}"
        ))),
    }
}

pub(crate) fn read_reply(input: &mut impl Read) -> io::Result<Reply> {
    let frame = read_frame(input)?;
    let mut payload = Payload(&frame);
    match payload.byte()? {
        LOADED => {
            let plugins = payload.list(|payload| {
                Ok(LoadedPlugin {
                    registrar: payload.string()?,
                    plugin_id: payload.string()?,
                    supports_k2: payload.byte()? != 0,
                    extensions: payload.list(|payload| Ok((payload.string()?, payload.u32()?)))?,
                })
            })?;
            Ok(Reply::Loaded(plugins))
        }
        UNLOADED => Ok(Reply::Unloaded),
        SESSION_OPENED => Ok(Reply::SessionOpened),
        SESSION_CLOSED => Ok(Reply::SessionClosed),
        RESOLVED => Ok(Reply::Resolved(payload.list(|payload| payload.string())?)),
        FAILED => Ok(Reply::Failed(payload.string()?)),
        tag => Err(malformed(format!("unknown reply {tag}"))),
    }
}

fn write_frame(out: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    let length = u32::try_from(payload.len()).map_err(|_| malformed("a frame over 4 GB".into()))?;
    out.write_all(&length.to_be_bytes())?;
    out.write_all(payload)?;
    out.flush()
}

fn read_frame(input: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut length = [0u8; 4];
    input.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length);
    if length > MAX_FRAME {
        return Err(malformed(format!("a frame of {length} bytes")));
    }
    let mut frame = vec![0u8; length as usize];
    input.read_exact(&mut frame)?;
    Ok(frame)
}

/// A path as the host reads it. kotlinc takes paths as strings, so one that is not UTF-8 cannot be
/// passed on unchanged.
fn utf8(path: &std::path::Path) -> io::Result<&str> {
    path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a UTF-8 path", path.display()),
        )
    })
}

fn put_callable(payload: &mut Vec<u8>, identity: &CallableIdentity) {
    match &identity.kind {
        CallableKind::Function { .. } => payload.push(0),
        CallableKind::Property { .. } => payload.push(1),
        CallableKind::Constructor => payload.push(2),
    }
    put_str(payload, &identity.package);
    put_list(payload, &identity.classes, |payload, class| {
        put_str(payload, class)
    });
    match &identity.kind {
        CallableKind::Function { name } | CallableKind::Property { name } => put_str(payload, name),
        CallableKind::Constructor => {}
    }
    payload.extend_from_slice(&identity.type_parameters.to_be_bytes());
    put_list(payload, &identity.context_parameters, put_type);
    match &identity.extension_receiver {
        Some(receiver) => {
            payload.push(1);
            put_type(payload, receiver);
        }
        None => payload.push(0),
    }
    put_list(payload, &identity.value_parameters, put_type);
}

fn put_class_name(payload: &mut Vec<u8>, class: &ClassName) {
    put_str(payload, &class.package);
    put_list(payload, &class.classes, |payload, name| {
        put_str(payload, name)
    });
}

fn put_type(payload: &mut Vec<u8>, ty: &TypeIdentity) {
    match &ty.classifier {
        Classifier::Class { class, arguments } => {
            payload.push(0);
            put_class_name(payload, class);
            put_list(payload, arguments, put_argument);
        }
        Classifier::TypeParameter { of_class, index } => {
            payload.push(1);
            payload.push(u8::from(*of_class));
            payload.extend_from_slice(&index.to_be_bytes());
        }
    }
    payload.push(u8::from(ty.nullable));
    put_list(payload, &ty.annotations, put_class_name);
}

fn put_argument(payload: &mut Vec<u8>, argument: &TypeArgument) {
    let (tag, ty) = match argument {
        TypeArgument::Star => (0, None),
        TypeArgument::Invariant(ty) => (1, Some(ty)),
        TypeArgument::In(ty) => (2, Some(ty)),
        TypeArgument::Out(ty) => (3, Some(ty)),
    };
    payload.push(tag);
    if let Some(ty) = ty {
        put_type(payload, ty);
    }
}

fn put_str(payload: &mut Vec<u8>, value: &str) {
    payload.extend_from_slice(&(value.len() as u32).to_be_bytes());
    payload.extend_from_slice(value.as_bytes());
}

fn put_list<T>(payload: &mut Vec<u8>, items: &[T], mut put: impl FnMut(&mut Vec<u8>, &T)) {
    payload.extend_from_slice(&(items.len() as u32).to_be_bytes());
    for item in items {
        put(payload, item);
    }
}

struct Payload<'a>(&'a [u8]);

impl Payload<'_> {
    fn take(&mut self, n: usize) -> io::Result<&[u8]> {
        if self.0.len() < n {
            return Err(malformed("a message ends early".into()));
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn byte(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> io::Result<u32> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn string(&mut self) -> io::Result<String> {
        let length = self.u32()? as usize;
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|_| malformed("a string that is not UTF-8".into()))
    }

    fn list<T>(
        &mut self,
        mut element: impl FnMut(&mut Self) -> io::Result<T>,
    ) -> io::Result<Vec<T>> {
        let count = self.u32()?;
        (0..count).map(|_| element(self)).collect()
    }
}

fn malformed(detail: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_load_request_is_framed_strings_in_order() {
        let mut bytes = Vec::new();
        let entries = ["a.jar".to_string()];
        let options = ["plugin:p:k=v".to_string()];
        write_request(
            &mut bytes,
            &Request::Load {
                entries: &entries,
                options: &options,
            },
        )
        .unwrap();
        let mut expected = vec![0, 0, 0, 34, LOAD, 0, 0, 0, 1, 0, 0, 0, 5];
        expected.extend_from_slice(b"a.jar");
        expected.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 12]);
        expected.extend_from_slice(b"plugin:p:k=v");
        assert_eq!(bytes, expected);
    }

    #[test]
    fn a_resolve_request_frames_each_identity_in_signature_order() {
        let any = TypeIdentity {
            classifier: Classifier::Class {
                class: ClassName {
                    package: "kotlin".into(),
                    classes: vec!["Any".into()],
                },
                arguments: vec![TypeArgument::Star],
            },
            nullable: true,
            annotations: Vec::new(),
        };
        let identity = CallableIdentity {
            kind: CallableKind::Function {
                name: "println".into(),
            },
            package: "kotlin.io".into(),
            classes: Vec::new(),
            type_parameters: 0,
            context_parameters: Vec::new(),
            extension_receiver: None,
            value_parameters: vec![any],
        };
        let mut bytes = Vec::new();
        write_request(&mut bytes, &Request::Resolve(&[identity])).unwrap();
        let mut payload = vec![RESOLVE, 0, 0, 0, 1, 0];
        put_str(&mut payload, "kotlin.io");
        payload.extend_from_slice(&[0, 0, 0, 0]);
        put_str(&mut payload, "println");
        // No type parameters, no context parameters, no extension receiver, one value parameter.
        payload.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0]);
        put_str(&mut payload, "kotlin");
        payload.extend_from_slice(&[0, 0, 0, 1]);
        put_str(&mut payload, "Any");
        // One star argument, nullable, no annotations.
        payload.extend_from_slice(&[0, 0, 0, 1, 0, 1, 0, 0, 0, 0]);
        let mut expected = (payload.len() as u32).to_be_bytes().to_vec();
        expected.extend_from_slice(&payload);
        assert_eq!(bytes, expected);
    }

    #[test]
    #[cfg(unix)]
    fn a_session_over_a_path_that_is_not_utf8_is_refused_before_it_is_sent() {
        use std::os::unix::ffi::OsStrExt;
        let request = SessionRequest {
            classpath: vec![std::ffi::OsStr::from_bytes(b"/lib/\xff.jar").into()],
            jdk_home: "/jdk".into(),
            module_name: "main".into(),
            language_version: "2.4".into(),
            api_version: "2.4".into(),
            jvm_target: "1.8".into(),
        };
        let mut bytes = Vec::new();
        let error = write_request(&mut bytes, &Request::OpenSession(&request)).unwrap_err();
        assert_eq!(
            (error.kind(), error.to_string(), bytes),
            (
                io::ErrorKind::InvalidInput,
                "/lib/\u{FFFD}.jar is not a UTF-8 path".to_string(),
                Vec::new()
            )
        );
    }

    #[test]
    fn a_loaded_reply_reads_back_every_field() {
        let mut payload = vec![LOADED];
        payload.extend_from_slice(&1u32.to_be_bytes());
        put_str(&mut payload, "org.example.Registrar");
        put_str(&mut payload, "org.example");
        payload.push(1);
        payload.extend_from_slice(&1u32.to_be_bytes());
        put_str(&mut payload, "org.jetbrains.kotlin.irGenerationExtension");
        payload.extend_from_slice(&2u32.to_be_bytes());
        let mut frame = Vec::new();
        write_frame(&mut frame, &payload).unwrap();
        assert_eq!(
            read_reply(&mut frame.as_slice()).unwrap(),
            Reply::Loaded(vec![LoadedPlugin {
                registrar: "org.example.Registrar".into(),
                plugin_id: "org.example".into(),
                supports_k2: true,
                extensions: vec![("org.jetbrains.kotlin.irGenerationExtension".into(), 2)],
            }])
        );
    }

    #[test]
    fn a_truncated_message_is_an_error_not_a_panic() {
        let mut frame = Vec::new();
        write_frame(&mut frame, &[FAILED, 0, 0, 0, 9, b'x']).unwrap();
        let error = read_reply(&mut frame.as_slice()).unwrap_err();
        assert_eq!(
            (error.kind(), error.to_string()),
            (
                io::ErrorKind::InvalidData,
                "a message ends early".to_string()
            )
        );
    }
}
