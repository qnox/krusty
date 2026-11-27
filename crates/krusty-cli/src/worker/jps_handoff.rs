//! Handing a target krusty did not compile to the build's own JPS builder.
//!
//! A krusty failure is a bug, and falling back is guidance for work: the build still succeeds
//! when the source is valid, and the action's output records why krusty did not compile the
//! target. The integration patch starts this worker with the command that runs the JPS builder
//! (`--jps-builder`, one argument at a time). After a failure the worker runs that builder as a
//! one-request persistent worker of its own and forwards the original request to it: the same
//! arguments, which name the action's flagfile, and the same inputs. The JPS builder speaks only
//! bazel's protobuf worker protocol, so the request and response are encoded here.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

use super::{WorkInput, WorkRequest, WorkResponse};

/// Why krusty did not compile a target, in the words the build log records. The kinds keep a bug
/// (an internal error) apart from work krusty has still to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// Every error is a native compiler plugin's refusal of source kotlinc compiles.
    Unimplemented,
    /// krusty reported errors; the JPS builder decides whether the source is valid.
    Errors,
    /// A panic or another internal error.
    Internal,
    /// The request's command line is one krusty does not accept.
    CommandLine,
}

impl FailureKind {
    pub fn why(self) -> &'static str {
        match self {
            Self::Unimplemented => "krusty does not implement everything the target uses",
            Self::Errors => "krusty reported errors",
            Self::Internal => "krusty failed with an internal error",
            Self::CommandLine => "krusty rejects the command line",
        }
    }
}

/// A failed compilation of one work request: its kind and krusty's rendered report.
#[derive(Debug, PartialEq, Eq)]
pub struct WorkFailure {
    pub kind: FailureKind,
    pub report: String,
}

impl WorkFailure {
    pub fn new(kind: FailureKind, report: impl Into<String>) -> Self {
        Self {
            kind,
            report: report.into(),
        }
    }
}

/// The command that starts the JPS builder as a persistent worker, without `--persistent_worker`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handoff {
    command: Vec<String>,
}

impl Handoff {
    pub fn new(command: Vec<String>) -> Result<Self, String> {
        if command.is_empty() {
            return Err("--jps-builder names no command".to_string());
        }
        Ok(Self { command })
    }

    /// Compile `request` with the JPS builder after krusty failed it. `note` is what krusty
    /// already had to say about the request; the response carries it, the line recording the
    /// handoff and krusty's report, then the JPS builder's own output and exit code.
    pub fn compile(
        &self,
        request: &WorkRequest,
        label: Option<&str>,
        failure: &WorkFailure,
        note: &str,
    ) -> WorkResponse {
        let target = label.unwrap_or("this target");
        let mut output = format!(
            "{note}krusty: target '{target}' is left to the JPS builder: {}\n{}",
            failure.kind.why(),
            failure.report
        );
        if !output.ends_with('\n') {
            output.push('\n');
        }
        let exit_code = match self.run(request) {
            Ok(response) => {
                output.push_str(&response.output);
                response.exit_code
            }
            Err(error) => {
                output.push_str(&format!("krusty: cannot run the JPS builder: {error}\n"));
                1
            }
        };
        WorkResponse {
            exit_code,
            output,
            request_id: request.request_id,
        }
    }

    fn run(&self, request: &WorkRequest) -> Result<JpsResponse, String> {
        let message = encode_request(request)?;
        let mut child = Command::new(&self.command[0])
            .args(&self.command[1..])
            .arg("--persistent_worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("starting `{}`: {error}", self.command[0]))?;
        // Closing the request stream after the one request ends the builder once that request is
        // answered: its loop finishes the requests it read before it stops.
        {
            let mut stdin = child.stdin.take().expect("piped stdin");
            let mut framed = Vec::new();
            write_varint(&mut framed, message.len() as u64);
            framed.extend_from_slice(&message);
            stdin
                .write_all(&framed)
                .map_err(|error| format!("sending the request: {error}"))?;
        }
        let mut stdout = Vec::new();
        child
            .stdout
            .take()
            .expect("piped stdout")
            .read_to_end(&mut stdout)
            .map_err(|error| format!("reading the response: {error}"))?;
        let status = child
            .wait()
            .map_err(|error| format!("waiting for the builder: {error}"))?;
        decode_response(&stdout).map_err(|error| format!("{error} (the builder exited {status})"))
    }
}

/// What the JPS builder answered.
#[derive(Debug, PartialEq, Eq)]
struct JpsResponse {
    exit_code: i32,
    output: String,
}

/// `WorkRequest` of bazel's `worker_protocol.proto`: `arguments` (1), `inputs` (2, each `path` 1
/// and `digest` 2) and `request_id` (3).
fn encode_request(request: &WorkRequest) -> Result<Vec<u8>, String> {
    let mut message = Vec::new();
    for argument in &request.arguments {
        write_bytes(&mut message, 1, argument.as_bytes());
    }
    for WorkInput { path, digest } in &request.inputs {
        let mut input = Vec::new();
        write_bytes(&mut input, 1, path.as_bytes());
        let digest = decode_base64(digest)
            .ok_or_else(|| format!("the digest of input `{path}` is not base64"))?;
        write_bytes(&mut input, 2, &digest);
        write_bytes(&mut message, 2, &input);
    }
    if request.request_id != 0 {
        write_varint(&mut message, 3 << 3);
        // An int32 field is a sign-extended varint.
        write_varint(&mut message, i64::from(request.request_id) as u64);
    }
    Ok(message)
}

/// The first length-delimited `WorkResponse` in `bytes`: `exit_code` (1) and `output` (2).
fn decode_response(bytes: &[u8]) -> Result<JpsResponse, String> {
    let mut cursor = bytes;
    let length = read_varint(&mut cursor).ok_or("the builder answered no response")? as usize;
    let mut message = cursor
        .get(..length)
        .ok_or("the builder's response is truncated")?;
    let mut response = JpsResponse {
        exit_code: 0,
        output: String::new(),
    };
    while !message.is_empty() {
        let key = read_varint(&mut message).ok_or("a malformed response field")?;
        match (key >> 3, key & 7) {
            (1, 0) => {
                response.exit_code =
                    read_varint(&mut message).ok_or("a malformed exit code")? as i32;
            }
            (2, 2) => {
                let text = read_length_delimited(&mut message).ok_or("a malformed output")?;
                response.output = String::from_utf8_lossy(text).into_owned();
            }
            (_, 0) => {
                read_varint(&mut message).ok_or("a malformed response field")?;
            }
            (_, 2) => {
                read_length_delimited(&mut message).ok_or("a malformed response field")?;
            }
            (_, wire) => return Err(format!("a response field of wire type {wire}")),
        }
    }
    Ok(response)
}

fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn write_bytes(out: &mut Vec<u8>, field: u64, bytes: &[u8]) {
    write_varint(out, (field << 3) | 2);
    write_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn read_varint(input: &mut &[u8]) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = input.split_first()?;
        *input = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn read_length_delimited<'a>(input: &mut &'a [u8]) -> Option<&'a [u8]> {
    let length = read_varint(input)? as usize;
    let (value, rest) = (input.get(..length)?, input.get(length..)?);
    *input = rest;
    Some(value)
}

/// Standard base64 with optional padding, as proto3's JSON mapping writes a `bytes` field.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let mut bits = 0u32;
    let mut count = 0;
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    for byte in text.trim_end_matches('=').bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(arguments: &[&str], inputs: &[(&str, &str)], request_id: i32) -> WorkRequest {
        WorkRequest {
            arguments: arguments.iter().map(|value| value.to_string()).collect(),
            inputs: inputs
                .iter()
                .map(|(path, digest)| WorkInput {
                    path: path.to_string(),
                    digest: digest.to_string(),
                })
                .collect(),
            request_id,
            cancel: false,
        }
    }

    #[test]
    fn a_request_is_encoded_as_the_worker_protocol_message() {
        let encoded = encode_request(&request(&["--flagfile=a"], &[("A.kt", "AQI=")], 5)).unwrap();
        assert_eq!(
            encoded,
            [
                &[0x0a, 12][..],
                b"--flagfile=a",
                &[0x12, 10, 0x0a, 4],
                b"A.kt",
                &[0x12, 2, 1, 2, 0x18, 5],
            ]
            .concat()
        );
    }

    #[test]
    fn a_negative_request_id_is_a_sign_extended_varint() {
        let encoded = encode_request(&request(&[], &[], -1)).unwrap();
        assert_eq!(
            encoded,
            [0x18, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]
        );
    }

    #[test]
    fn an_input_digest_that_is_not_base64_is_refused() {
        assert_eq!(
            encode_request(&request(&[], &[("A.kt", "*")], 0)),
            Err("the digest of input `A.kt` is not base64".to_string())
        );
    }

    #[test]
    fn a_response_is_decoded_past_fields_it_does_not_read() {
        let message = [&[0x08, 2, 0x18, 7, 0x12, 3][..], b"bad", &[0x22, 0]].concat();
        let framed = [&[message.len() as u8][..], &message].concat();
        assert_eq!(
            decode_response(&framed),
            Ok(JpsResponse {
                exit_code: 2,
                output: "bad".to_string(),
            })
        );
        assert_eq!(
            decode_response(&[]),
            Err("the builder answered no response".to_string())
        );
        assert_eq!(
            decode_response(&[5, 0x08]),
            Err("the builder's response is truncated".to_string())
        );
    }

    #[test]
    fn base64_decodes_with_and_without_padding() {
        assert_eq!(decode_base64("aGk="), Some(b"hi".to_vec()));
        assert_eq!(decode_base64("aGk"), Some(b"hi".to_vec()));
        assert_eq!(decode_base64(""), Some(Vec::new()));
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_target_is_compiled_by_the_jps_builder_and_the_reason_recorded() {
        // A stand-in builder: it reads the request, then answers exit code 0 and "built\n".
        let handoff = Handoff::new(vec![
            "sh".to_string(),
            "-c".to_string(),
            r"cat >/dev/null; printf '\012\010\000\022\006built\n'".to_string(),
        ])
        .unwrap();
        let response = handoff.compile(
            &request(&["--flagfile=a"], &[], 4),
            Some("//lib:lib"),
            &WorkFailure::new(
                FailureKind::Internal,
                "krusty: internal compiler panic: x\n",
            ),
            "warning: w\n",
        );
        assert_eq!(
            response,
            WorkResponse {
                exit_code: 0,
                output: "warning: w\nkrusty: target '//lib:lib' is left to the JPS builder: \
                         krusty failed with an internal error\nkrusty: internal compiler panic: \
                         x\nbuilt\n"
                    .to_string(),
                request_id: 4,
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_builder_that_cannot_answer_fails_the_target_with_both_reports() {
        let handoff = Handoff::new(vec![
            "sh".to_string(),
            "-c".to_string(),
            "cat >/dev/null".to_string(),
        ])
        .unwrap();
        let response = handoff.compile(
            &request(&[], &[], 0),
            None,
            &WorkFailure::new(FailureKind::Errors, "A.kt:1:1: error: e"),
            "",
        );
        assert_eq!(response.exit_code, 1);
        assert_eq!(
            response.output,
            "krusty: target 'this target' is left to the JPS builder: krusty reported errors\n\
             A.kt:1:1: error: e\nkrusty: cannot run the JPS builder: the builder answered no \
             response (the builder exited exit status: 0)\n"
        );
    }
}
