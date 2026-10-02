//! JSON-RPC response delivery and bounded diagnostic-stream scheduling.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use serde_json::Value;

use super::diagnostic_page::WorkspaceDiagnosticStream;
use super::implementation::{json_io, write_framed, Incoming};
use super::output_queue::OutputQueue;

const DIAGNOSTIC_STREAM_DRAIN_POLL: Duration = Duration::from_millis(1);
const MAX_INPUTS_BUFFERED_DURING_DIAGNOSTIC_STREAM: usize = 32;

pub struct Dispatch {
    pub messages: Vec<Value>,
    diagnostic_stream: Option<WorkspaceDiagnosticStream>,
    pub exit: bool,
    pub exit_code: i32,
}

impl Dispatch {
    pub(crate) fn messages(messages: Vec<Value>) -> Self {
        Self {
            messages,
            diagnostic_stream: None,
            exit: false,
            exit_code: 0,
        }
    }

    pub(super) fn diagnostic_stream(stream: WorkspaceDiagnosticStream) -> Self {
        Self {
            messages: Vec::new(),
            diagnostic_stream: Some(stream),
            exit: false,
            exit_code: 0,
        }
    }

    pub(crate) fn none() -> Self {
        Self::messages(Vec::new())
    }

    pub(super) fn exit(exit_code: i32) -> Self {
        Self {
            messages: Vec::new(),
            diagnostic_stream: None,
            exit: true,
            exit_code,
        }
    }

    #[cfg(test)]
    pub(crate) fn into_messages_for_test(mut self) -> Vec<Value> {
        if let Some(mut stream) = self.diagnostic_stream.take() {
            while let Some(message) = stream.next_message() {
                self.messages.push(message);
            }
        }
        self.messages
    }
}

fn write_value<W: Write>(writer: &mut W, value: &Value) -> io::Result<()> {
    let encoded = serde_json::to_vec(value).map_err(json_io)?;
    write_framed(writer, &encoded)
}

pub(super) fn dispatch_sync<W: Write>(
    writer: &mut W,
    mut dispatch: Dispatch,
) -> io::Result<Option<i32>> {
    for response in dispatch.messages {
        write_value(writer, &response)?;
    }
    if let Some(mut stream) = dispatch.diagnostic_stream.take() {
        while let Some(response) = stream.next_message() {
            write_value(writer, &response)?;
        }
    }
    Ok(dispatch.exit.then_some(dispatch.exit_code))
}

#[derive(Default)]
pub(super) struct AsyncResponseDelivery {
    stream: Option<WorkspaceDiagnosticStream>,
    incoming_closed: bool,
}

impl AsyncResponseDelivery {
    pub(super) fn is_active(&self) -> bool {
        self.stream.is_some()
    }

    pub(super) fn accept<W: Write>(
        &mut self,
        writer: &mut W,
        mut dispatch: Dispatch,
    ) -> io::Result<Option<i32>> {
        for response in dispatch.messages {
            write_value(writer, &response)?;
        }
        if let Some(stream) = dispatch.diagnostic_stream.take() {
            if self.stream.replace(stream).is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "overlapping workspace diagnostic streams",
                ));
            }
        }
        Ok(dispatch.exit.then_some(dispatch.exit_code))
    }

    /// Makes one bounded delivery step. State-changing events remain queued until the immutable
    /// snapshot reaches its sole terminal response; only its matching cancellation is consumed.
    pub(super) fn advance(
        &mut self,
        writer: &mut OutputQueue,
        incoming: &Receiver<Incoming>,
        pending: &mut VecDeque<Incoming>,
    ) -> io::Result<()> {
        let Some(stream) = self.stream.as_mut() else {
            return Ok(());
        };
        while pending.len() < MAX_INPUTS_BUFFERED_DURING_DIAGNOSTIC_STREAM {
            match incoming.try_recv() {
                Ok(event) => retain_or_cancel(stream, pending, event),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.incoming_closed = true;
                    break;
                }
            }
        }
        if writer.is_idle()? {
            if let Some(message) = stream.next_message() {
                write_value(writer, &message)?;
            } else {
                self.stream = None;
            }
            return Ok(());
        }
        if pending.len() >= MAX_INPUTS_BUFFERED_DURING_DIAGNOSTIC_STREAM || self.incoming_closed {
            std::thread::sleep(DIAGNOSTIC_STREAM_DRAIN_POLL);
            return Ok(());
        }
        match incoming.recv_timeout(DIAGNOSTIC_STREAM_DRAIN_POLL) {
            Ok(event) => retain_or_cancel(stream, pending, event),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => self.incoming_closed = true,
        }
        Ok(())
    }
}

fn retain_or_cancel(
    stream: &mut WorkspaceDiagnosticStream,
    pending: &mut VecDeque<Incoming>,
    event: Incoming,
) {
    if let Incoming::Message(message) = &event {
        if message.get("method").and_then(Value::as_str) == Some("$/cancelRequest")
            && message.pointer("/params/id") == Some(stream.request_id())
        {
            stream.cancel();
            return;
        }
    }
    pending.push_back(event);
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    use serde_json::json;

    use super::*;
    use crate::server::diagnostic_page::{
        workspace_diagnostic_messages, workspace_diagnostic_response, WorkspaceDiagnosticResponse,
    };

    #[derive(Clone, Default)]
    struct GatedWriter {
        state: Arc<(Mutex<GatedWriterState>, Condvar)>,
    }

    #[derive(Default)]
    struct GatedWriterState {
        started: bool,
        open: bool,
        bytes: Vec<u8>,
    }

    impl GatedWriter {
        fn wait_started(&self) {
            let deadline = Instant::now() + Duration::from_secs(2);
            let (lock, changed) = &*self.state;
            let mut state = lock.lock().expect("gated writer");
            while !state.started {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .expect("writer did not start");
                let (next, timeout) = changed
                    .wait_timeout(state, remaining)
                    .expect("gated writer wait");
                state = next;
                assert!(
                    !timeout.timed_out() || state.started,
                    "writer did not start"
                );
            }
        }

        fn open(&self) {
            let (lock, changed) = &*self.state;
            lock.lock().expect("gated writer").open = true;
            changed.notify_all();
        }

        fn bytes(&self) -> Vec<u8> {
            let (lock, _) = &*self.state;
            lock.lock().expect("gated writer").bytes.clone()
        }
    }

    impl Write for GatedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let (lock, changed) = &*self.state;
            let mut state = lock.lock().expect("gated writer");
            state.started = true;
            changed.notify_all();
            while !state.open {
                state = changed.wait(state).expect("gated writer wait");
            }
            state.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn decode_frames(mut bytes: &[u8]) -> Vec<Value> {
        let mut messages = Vec::new();
        while !bytes.is_empty() {
            let header_end = bytes
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .expect("complete LSP header");
            let header = std::str::from_utf8(&bytes[..header_end]).expect("UTF-8 header");
            let length = header
                .strip_prefix("Content-Length: ")
                .expect("content-length header")
                .parse::<usize>()
                .expect("content length");
            let body_start = header_end + 4;
            let body_end = body_start + length;
            messages.push(serde_json::from_slice(&bytes[body_start..body_end]).expect("JSON body"));
            bytes = &bytes[body_end..];
        }
        messages
    }

    #[test]
    fn maximum_stream_observes_cancellation_between_blocked_pages() {
        let id = json!("workspace/diagnostic/large");
        let token = json!("workspace/diagnostic/progress");
        let payload = "m".repeat(8 * 1024);
        let items = (0..992)
            .map(|index| json!({"index": index, "payload": payload}))
            .collect::<Vec<_>>();
        let expected_complete = items
            .chunks(31)
            .map(|page| {
                json!({
                    "jsonrpc": "2.0",
                    "method": "$/progress",
                    "params": {
                        "token": token,
                        "value": {"items": page}
                    }
                })
            })
            .chain(std::iter::once(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"items": []}
            })))
            .collect::<Vec<_>>();
        assert_eq!(expected_complete.len(), 33);
        assert_eq!(
            workspace_diagnostic_messages(id.clone(), items.clone(), Some(&token)),
            expected_complete
        );

        let WorkspaceDiagnosticResponse::Stream(stream) =
            workspace_diagnostic_response(id.clone(), items.clone(), Some(&token))
        else {
            panic!("maximum bounded response must stream");
        };
        let inner = GatedWriter::default();
        let capture = inner.clone();
        let mut writer = OutputQueue::spawn(inner).expect("output queue");
        let mut delivery = AsyncResponseDelivery::default();
        assert_eq!(
            delivery
                .accept(&mut writer, Dispatch::diagnostic_stream(stream))
                .unwrap(),
            None
        );
        let (sender, incoming) = mpsc::channel();
        let mut pending = VecDeque::new();
        delivery
            .advance(&mut writer, &incoming, &mut pending)
            .unwrap();
        capture.wait_started();
        sender
            .send(Incoming::Message(json!({
                "jsonrpc": "2.0",
                "method": "$/cancelRequest",
                "params": {"id": id}
            })))
            .unwrap();
        delivery
            .advance(&mut writer, &incoming, &mut pending)
            .unwrap();
        capture.open();
        let deadline = Instant::now() + Duration::from_secs(2);
        while delivery.is_active() {
            assert!(
                Instant::now() < deadline,
                "stream did not reach its terminal"
            );
            delivery
                .advance(&mut writer, &incoming, &mut pending)
                .unwrap();
        }
        assert!(pending.is_empty());
        assert_eq!(writer.finish(Ok(0)).unwrap(), 0);
        assert_eq!(
            decode_frames(&capture.bytes()),
            vec![
                json!({
                    "jsonrpc": "2.0",
                    "method": "$/progress",
                    "params": {
                        "token": token,
                        "value": {"items": &items[..31]}
                    }
                }),
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32800,
                        "message": "request cancelled"
                    }
                })
            ]
        );
    }
}
