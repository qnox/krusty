//! JSON-RPC response delivery and bounded diagnostic-stream scheduling.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use serde_json::Value;

use super::diagnostic_page::WorkspaceDiagnosticStream;
use super::implementation::{json_io, write_framed, Incoming, INPUT_QUEUE_CAPACITY};
use super::output_queue::OutputQueue;

const DIAGNOSTIC_STREAM_DRAIN_POLL: Duration = Duration::from_millis(1);
const MAX_INPUTS_BUFFERED_DURING_DIAGNOSTIC_STREAM: usize = 32;
/// Once the normal pending deque is full, retain at most one production input-channel's worth of
/// additional events while looking for a cancellation. This is one aggregate bound across
/// consecutive streams: events restored by an earlier stream already consume the next stream's
/// capacity.
const MAX_INPUTS_RETAINED_DURING_DIAGNOSTIC_STREAM: usize =
    MAX_INPUTS_BUFFERED_DURING_DIAGNOSTIC_STREAM + INPUT_QUEUE_CAPACITY;

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
    /// A production input-channel's worth of events pulled after the normal pending deque fills.
    /// Their FIFO order follows the pending deque and is restored before normal dispatch resumes.
    deferred: VecDeque<Incoming>,
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
    /// A full pending deque still observes a matching cancellation behind the bounded input
    /// channel. If that complete lookahead fills with unrelated events, the stream fails closed
    /// before another page while retaining all events in their original order.
    pub(super) fn advance(
        &mut self,
        writer: &mut OutputQueue,
        incoming: &Receiver<Incoming>,
        pending: &mut VecDeque<Incoming>,
    ) -> io::Result<()> {
        if self.stream.is_none() {
            return Ok(());
        }
        {
            let AsyncResponseDelivery {
                stream,
                incoming_closed,
                deferred,
            } = &mut *self;
            let Some(stream) = stream.as_mut() else {
                return Ok(());
            };
            observe_incoming(deferred, incoming_closed, stream, incoming, pending);
        }
        if writer.is_idle()? {
            let message = self
                .stream
                .as_mut()
                .and_then(WorkspaceDiagnosticStream::next_message);
            if let Some(message) = message {
                write_value(writer, &message)?;
            } else {
                self.stream = None;
                pending.append(&mut self.deferred);
            }
            return Ok(());
        }
        if pending.len() >= MAX_INPUTS_BUFFERED_DURING_DIAGNOSTIC_STREAM
            || !self.deferred.is_empty()
            || self.incoming_closed
        {
            std::thread::sleep(DIAGNOSTIC_STREAM_DRAIN_POLL);
            return Ok(());
        }
        match incoming.recv_timeout(DIAGNOSTIC_STREAM_DRAIN_POLL) {
            Ok(event) => {
                let Some(stream) = self.stream.as_mut() else {
                    return Ok(());
                };
                retain_or_cancel(stream, pending, event);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => self.incoming_closed = true,
        }
        Ok(())
    }
}

fn observe_incoming(
    deferred: &mut VecDeque<Incoming>,
    incoming_closed: &mut bool,
    stream: &mut WorkspaceDiagnosticStream,
    incoming: &Receiver<Incoming>,
    pending: &mut VecDeque<Incoming>,
) {
    cancel_from_pending(stream, pending);
    if retained_inputs(pending, deferred) >= MAX_INPUTS_RETAINED_DURING_DIAGNOSTIC_STREAM {
        stream.cancel_for_delivery_backlog();
        return;
    }
    loop {
        match incoming.try_recv() {
            Ok(event) => {
                if is_matching_cancellation(stream, &event) {
                    stream.cancel();
                    break;
                }
                if deferred.is_empty()
                    && pending.len() < MAX_INPUTS_BUFFERED_DURING_DIAGNOSTIC_STREAM
                {
                    pending.push_back(event);
                } else {
                    deferred.push_back(event);
                }
                if retained_inputs(pending, deferred)
                    >= MAX_INPUTS_RETAINED_DURING_DIAGNOSTIC_STREAM
                {
                    stream.cancel_for_delivery_backlog();
                    break;
                }
            }
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                *incoming_closed = true;
                break;
            }
        }
    }
}

fn retained_inputs(pending: &VecDeque<Incoming>, deferred: &VecDeque<Incoming>) -> usize {
    pending.len().saturating_add(deferred.len())
}

fn cancel_from_pending(stream: &mut WorkspaceDiagnosticStream, pending: &mut VecDeque<Incoming>) {
    let Some(index) = pending
        .iter()
        .position(|event| is_matching_cancellation(stream, event))
    else {
        return;
    };
    pending.remove(index);
    stream.cancel();
}

fn retain_or_cancel(
    stream: &mut WorkspaceDiagnosticStream,
    pending: &mut VecDeque<Incoming>,
    event: Incoming,
) {
    if is_matching_cancellation(stream, &event) {
        stream.cancel();
        return;
    }
    pending.push_back(event);
}

fn is_matching_cancellation(stream: &WorkspaceDiagnosticStream, event: &Incoming) -> bool {
    let Incoming::Message(message) = event else {
        return false;
    };
    message.get("method").and_then(Value::as_str) == Some("$/cancelRequest")
        && message.pointer("/params/id") == Some(stream.request_id())
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

    fn wait_until_idle(writer: &mut OutputQueue) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !writer.is_idle().expect("output queue state") {
            assert!(
                Instant::now() < deadline,
                "output queue did not become idle"
            );
            std::thread::yield_now();
        }
    }

    fn configuration_event(index: usize) -> Value {
        json!({
            "jsonrpc": "2.0",
            "method": "workspace/didChangeConfiguration",
            "params": {"index": index}
        })
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

    #[test]
    fn a_buffered_matching_cancel_precedes_the_next_page_and_keeps_other_events() {
        use crate::server::output_queue::SharedWriter;

        let id = json!("workspace/diagnostic/buffered");
        let token = json!("workspace/diagnostic/progress");
        let items = (0..80)
            .map(|index| json!({"index": index, "payload": "m".repeat(8 * 1024)}))
            .collect::<Vec<_>>();
        let WorkspaceDiagnosticResponse::Stream(stream) =
            workspace_diagnostic_response(id.clone(), items, Some(&token))
        else {
            panic!("oversized bounded response must stream");
        };
        let (inner, captured) = SharedWriter::recording();
        let mut writer = OutputQueue::spawn(inner).expect("output queue");
        let mut delivery = AsyncResponseDelivery::default();
        assert_eq!(
            delivery
                .accept(&mut writer, Dispatch::diagnostic_stream(stream))
                .unwrap(),
            None
        );

        let normal = json!({
            "jsonrpc": "2.0",
            "method": "workspace/didChangeConfiguration",
            "params": {"settings": {}}
        });
        let mut pending = VecDeque::from([
            Incoming::Message(normal.clone()),
            Incoming::Message(json!({
                "jsonrpc": "2.0",
                "method": "$/cancelRequest",
                "params": {"id": id}
            })),
        ]);
        let (_sender, incoming) = mpsc::channel();
        let deadline = Instant::now() + Duration::from_secs(2);
        while delivery.is_active() {
            assert!(
                Instant::now() < deadline,
                "buffered cancellation did not reach its terminal"
            );
            delivery
                .advance(&mut writer, &incoming, &mut pending)
                .unwrap();
        }

        assert_eq!(pending.len(), 1);
        let Some(Incoming::Message(retained)) = pending.pop_front() else {
            panic!("the unrelated buffered event must remain queued");
        };
        assert_eq!(retained, normal);
        assert_eq!(writer.finish(Ok(0)).unwrap(), 0);
        assert_eq!(
            decode_frames(&captured.lock().expect("captured output")),
            vec![json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32800,
                    "message": "request cancelled"
                }
            })]
        );
    }

    #[test]
    fn a_full_pending_deque_still_observes_a_queued_matching_cancel() {
        use crate::server::output_queue::SharedWriter;

        let id = json!("workspace/diagnostic/full");
        let token = json!("workspace/diagnostic/progress");
        let items = (0..80)
            .map(|index| json!({"index": index, "payload": "m".repeat(8 * 1024)}))
            .collect::<Vec<_>>();
        let WorkspaceDiagnosticResponse::Stream(stream) =
            workspace_diagnostic_response(id.clone(), items, Some(&token))
        else {
            panic!("oversized bounded response must stream");
        };
        let (inner, captured) = SharedWriter::recording();
        let mut writer = OutputQueue::spawn(inner).expect("output queue");
        let mut delivery = AsyncResponseDelivery::default();
        assert_eq!(
            delivery
                .accept(&mut writer, Dispatch::diagnostic_stream(stream))
                .unwrap(),
            None
        );

        let pending_events = (0..MAX_INPUTS_BUFFERED_DURING_DIAGNOSTIC_STREAM)
            .map(|index| {
                json!({
                    "jsonrpc": "2.0",
                    "method": "workspace/didChangeConfiguration",
                    "params": {"index": index}
                })
            })
            .collect::<Vec<_>>();
        let mut pending = pending_events
            .iter()
            .cloned()
            .map(Incoming::Message)
            .collect::<VecDeque<_>>();
        let (sender, incoming) = mpsc::channel();
        sender
            .send(Incoming::Message(json!({
                "jsonrpc": "2.0",
                "method": "$/cancelRequest",
                "params": {"id": id}
            })))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while delivery.is_active() {
            assert!(
                Instant::now() < deadline,
                "queued cancellation did not reach its terminal"
            );
            delivery
                .advance(&mut writer, &incoming, &mut pending)
                .unwrap();
        }

        assert_eq!(pending.len(), pending_events.len());
        for (event, expected) in pending.iter().zip(&pending_events) {
            let Incoming::Message(message) = event else {
                panic!("unrelated buffered events must remain queued in order");
            };
            assert_eq!(message, expected);
        }
        assert_eq!(writer.finish(Ok(0)).unwrap(), 0);
        assert_eq!(
            decode_frames(&captured.lock().expect("captured output")),
            vec![json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32800,
                    "message": "request cancelled"
                }
            })]
        );
    }

    #[test]
    fn full_pending_and_one_unrelated_event_still_observe_the_following_cancel() {
        use crate::server::output_queue::SharedWriter;

        let id = json!("workspace/diagnostic/full-with-unrelated");
        let token = json!("workspace/diagnostic/progress");
        let items = (0..80)
            .map(|index| json!({"index": index, "payload": "m".repeat(8 * 1024)}))
            .collect::<Vec<_>>();
        let WorkspaceDiagnosticResponse::Stream(stream) =
            workspace_diagnostic_response(id.clone(), items.clone(), Some(&token))
        else {
            panic!("oversized bounded response must stream");
        };
        let (inner, captured) = SharedWriter::recording();
        let mut writer = OutputQueue::spawn(inner).expect("output queue");
        let mut delivery = AsyncResponseDelivery::default();
        assert_eq!(
            delivery
                .accept(&mut writer, Dispatch::diagnostic_stream(stream))
                .unwrap(),
            None
        );

        let (_empty_sender, empty_incoming) = mpsc::sync_channel(INPUT_QUEUE_CAPACITY);
        let mut pending = VecDeque::new();
        delivery
            .advance(&mut writer, &empty_incoming, &mut pending)
            .unwrap();
        wait_until_idle(&mut writer);

        let mut expected_events = (0..MAX_INPUTS_BUFFERED_DURING_DIAGNOSTIC_STREAM)
            .map(configuration_event)
            .collect::<Vec<_>>();
        pending.extend(expected_events.iter().cloned().map(Incoming::Message));
        let unrelated = configuration_event(MAX_INPUTS_BUFFERED_DURING_DIAGNOSTIC_STREAM);
        expected_events.push(unrelated.clone());
        let (sender, incoming) = mpsc::sync_channel(INPUT_QUEUE_CAPACITY);
        sender.send(Incoming::Message(unrelated)).unwrap();
        sender
            .send(Incoming::Message(json!({
                "jsonrpc": "2.0",
                "method": "$/cancelRequest",
                "params": {"id": id}
            })))
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        while delivery.is_active() {
            assert!(
                Instant::now() < deadline,
                "queued cancellation did not reach its terminal"
            );
            delivery
                .advance(&mut writer, &incoming, &mut pending)
                .unwrap();
        }

        let retained = pending
            .iter()
            .map(|event| match event {
                Incoming::Message(message) => message.clone(),
                _ => panic!("only messages were queued"),
            })
            .collect::<Vec<_>>();
        assert_eq!(retained, expected_events);
        assert_eq!(writer.finish(Ok(0)).unwrap(), 0);
        assert_eq!(
            decode_frames(&captured.lock().expect("captured output")),
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

    #[test]
    fn full_pending_and_full_lookahead_fail_closed_without_reordering_events() {
        use crate::server::output_queue::SharedWriter;

        let id = json!("workspace/diagnostic/input-capacity");
        let token = json!("workspace/diagnostic/progress");
        let items = (0..80)
            .map(|index| json!({"index": index, "payload": "m".repeat(8 * 1024)}))
            .collect::<Vec<_>>();
        let WorkspaceDiagnosticResponse::Stream(stream) =
            workspace_diagnostic_response(id.clone(), items.clone(), Some(&token))
        else {
            panic!("oversized bounded response must stream");
        };
        let (inner, captured) = SharedWriter::recording();
        let mut writer = OutputQueue::spawn(inner).expect("output queue");
        let mut delivery = AsyncResponseDelivery::default();
        assert_eq!(
            delivery
                .accept(&mut writer, Dispatch::diagnostic_stream(stream))
                .unwrap(),
            None
        );

        let (_empty_sender, empty_incoming) = mpsc::sync_channel(INPUT_QUEUE_CAPACITY);
        let mut pending = VecDeque::new();
        delivery
            .advance(&mut writer, &empty_incoming, &mut pending)
            .unwrap();
        wait_until_idle(&mut writer);

        let expected_events = (0..MAX_INPUTS_RETAINED_DURING_DIAGNOSTIC_STREAM)
            .map(configuration_event)
            .collect::<Vec<_>>();
        pending.extend(
            expected_events[..MAX_INPUTS_BUFFERED_DURING_DIAGNOSTIC_STREAM]
                .iter()
                .cloned()
                .map(Incoming::Message),
        );
        let (sender, incoming) = mpsc::sync_channel(INPUT_QUEUE_CAPACITY);
        for event in &expected_events[MAX_INPUTS_BUFFERED_DURING_DIAGNOSTIC_STREAM..] {
            sender.send(Incoming::Message(event.clone())).unwrap();
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        while delivery.is_active() {
            assert!(
                Instant::now() < deadline,
                "full delivery lookahead did not reach its terminal"
            );
            delivery
                .advance(&mut writer, &incoming, &mut pending)
                .unwrap();
        }

        let retained = pending
            .iter()
            .map(|event| match event {
                Incoming::Message(message) => message.clone(),
                _ => panic!("only messages were queued"),
            })
            .collect::<Vec<_>>();
        assert_eq!(retained, expected_events);
        assert_eq!(writer.finish(Ok(0)).unwrap(), 0);
        assert_eq!(
            decode_frames(&captured.lock().expect("captured output")),
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
                        "code": -32802,
                        "message": "workspace diagnostic delivery input queue is full",
                        "data": {"retriggerRequest": false}
                    }
                })
            ]
        );
    }

    #[test]
    fn chained_streams_share_one_retained_input_bound() {
        use crate::server::output_queue::SharedWriter;

        let first_id = json!("workspace/diagnostic/first");
        let second_id = json!("workspace/diagnostic/second");
        let first_token = json!("workspace/diagnostic/first-progress");
        let second_token = json!("workspace/diagnostic/second-progress");
        let items = (0..80)
            .map(|index| json!({"index": index, "payload": "m".repeat(8 * 1024)}))
            .collect::<Vec<_>>();
        let WorkspaceDiagnosticResponse::Stream(first) =
            workspace_diagnostic_response(first_id.clone(), items.clone(), Some(&first_token))
        else {
            panic!("oversized bounded response must stream");
        };
        let (inner, captured) = SharedWriter::recording();
        let mut writer = OutputQueue::spawn(inner).expect("output queue");
        let mut delivery = AsyncResponseDelivery::default();
        assert_eq!(
            delivery
                .accept(&mut writer, Dispatch::diagnostic_stream(first))
                .unwrap(),
            None
        );

        let (_empty_sender, empty_incoming) = mpsc::sync_channel(INPUT_QUEUE_CAPACITY);
        let mut pending = VecDeque::new();
        delivery
            .advance(&mut writer, &empty_incoming, &mut pending)
            .unwrap();
        wait_until_idle(&mut writer);

        let second_request = json!({
            "jsonrpc": "2.0",
            "id": second_id,
            "method": "workspace/diagnostic",
            "params": {"partialResultToken": second_token}
        });
        let mut expected_retained = vec![second_request.clone()];
        expected_retained
            .extend((1..MAX_INPUTS_BUFFERED_DURING_DIAGNOSTIC_STREAM).map(configuration_event));
        pending.extend(expected_retained.iter().cloned().map(Incoming::Message));
        let first_deferred = (MAX_INPUTS_BUFFERED_DURING_DIAGNOSTIC_STREAM
            ..MAX_INPUTS_RETAINED_DURING_DIAGNOSTIC_STREAM)
            .map(configuration_event)
            .collect::<Vec<_>>();
        expected_retained.extend(first_deferred.iter().cloned());
        let (first_sender, first_incoming) = mpsc::sync_channel(INPUT_QUEUE_CAPACITY);
        for event in first_deferred {
            first_sender.send(Incoming::Message(event)).unwrap();
        }
        let first_deadline = Instant::now() + Duration::from_secs(2);
        while delivery.is_active() {
            assert!(
                Instant::now() < first_deadline,
                "the first saturated stream did not reach its terminal"
            );
            delivery
                .advance(&mut writer, &first_incoming, &mut pending)
                .unwrap();
        }
        assert_eq!(pending.len(), MAX_INPUTS_RETAINED_DURING_DIAGNOSTIC_STREAM);

        let Some(Incoming::Message(next_request)) = pending.pop_front() else {
            panic!("the next retained event must be the second diagnostic request");
        };
        assert_eq!(next_request, second_request);
        expected_retained.remove(0);
        let WorkspaceDiagnosticResponse::Stream(second) =
            workspace_diagnostic_response(second_id.clone(), items.clone(), Some(&second_token))
        else {
            panic!("the second oversized bounded response must stream");
        };
        assert_eq!(
            delivery
                .accept(&mut writer, Dispatch::diagnostic_stream(second))
                .unwrap(),
            None
        );

        let second_lookahead = (MAX_INPUTS_RETAINED_DURING_DIAGNOSTIC_STREAM
            ..MAX_INPUTS_RETAINED_DURING_DIAGNOSTIC_STREAM + INPUT_QUEUE_CAPACITY)
            .map(configuration_event)
            .collect::<Vec<_>>();
        expected_retained.push(second_lookahead[0].clone());
        let (second_sender, second_incoming) = mpsc::sync_channel(INPUT_QUEUE_CAPACITY);
        for event in &second_lookahead {
            second_sender
                .send(Incoming::Message(event.clone()))
                .unwrap();
        }
        let second_deadline = Instant::now() + Duration::from_secs(2);
        while delivery.is_active() {
            assert!(
                Instant::now() < second_deadline,
                "the chained stream did not fail closed"
            );
            delivery
                .advance(&mut writer, &second_incoming, &mut pending)
                .unwrap();
        }

        let retained = pending
            .iter()
            .map(|event| match event {
                Incoming::Message(message) => message.clone(),
                _ => panic!("only messages were queued"),
            })
            .collect::<Vec<_>>();
        assert_eq!(retained, expected_retained);
        assert_eq!(retained.len(), MAX_INPUTS_RETAINED_DURING_DIAGNOSTIC_STREAM);
        let unread = second_incoming
            .try_iter()
            .map(|event| match event {
                Incoming::Message(message) => message,
                _ => panic!("only messages were queued"),
            })
            .collect::<Vec<_>>();
        assert_eq!(unread, second_lookahead[1..]);

        assert_eq!(writer.finish(Ok(0)).unwrap(), 0);
        assert_eq!(
            decode_frames(&captured.lock().expect("captured output")),
            vec![
                json!({
                    "jsonrpc": "2.0",
                    "method": "$/progress",
                    "params": {
                        "token": first_token,
                        "value": {"items": &items[..31]}
                    }
                }),
                json!({
                    "jsonrpc": "2.0",
                    "id": first_id,
                    "error": {
                        "code": -32802,
                        "message": "workspace diagnostic delivery input queue is full",
                        "data": {"retriggerRequest": false}
                    }
                }),
                json!({
                    "jsonrpc": "2.0",
                    "id": second_id,
                    "error": {
                        "code": -32802,
                        "message": "workspace diagnostic delivery input queue is full",
                        "data": {"retriggerRequest": false}
                    }
                })
            ]
        );
    }
}
