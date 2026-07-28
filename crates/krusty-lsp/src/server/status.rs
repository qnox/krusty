use serde_json::{json, Value};

use super::engine::ServerStatus;

#[derive(Default)]
pub(crate) struct StatusReporter {
    supported: bool,
    token: Option<String>,
    next_id: u64,
}

impl StatusReporter {
    pub(crate) fn set_supported(&mut self, supported: bool) {
        self.supported = supported;
    }

    pub(crate) fn report(&mut self, status: ServerStatus) -> Vec<Value> {
        if !self.supported {
            return Vec::new();
        }
        match status {
            ServerStatus::Working(message) => self.report_working(message),
            ServerStatus::Ready => self.finish(),
        }
    }

    fn report_working(&mut self, message: String) -> Vec<Value> {
        if let Some(token) = &self.token {
            return vec![progress_notification(
                token,
                json!({ "kind": "report", "message": message }),
            )];
        }

        let token = format!("krusty/status/{}", self.next_id);
        let request_id = format!("krusty/status/{}/create", self.next_id);
        self.next_id += 1;
        let messages = vec![
            json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "window/workDoneProgress/create",
                "params": { "token": token },
            }),
            progress_notification(
                &token,
                json!({ "kind": "begin", "title": "krusty", "message": message }),
            ),
        ];
        self.token = Some(token);
        messages
    }

    pub(crate) fn finish(&mut self) -> Vec<Value> {
        self.token
            .take()
            .map(|token| progress_notification(&token, json!({ "kind": "end" })))
            .into_iter()
            .collect()
    }
}

fn progress_notification(token: &str, value: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "$/progress",
        "params": { "token": token, "value": value },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_one_work_done_lifecycle() {
        let mut reporter = StatusReporter::default();
        reporter.set_supported(true);

        let begin = reporter.report(ServerStatus::Working("Loading project".into()));
        assert_eq!(begin.len(), 2);
        assert_eq!(begin[0]["method"], "window/workDoneProgress/create");
        assert_eq!(begin[1]["params"]["value"]["kind"], "begin");

        let update = reporter.report(ServerStatus::Working("Analyzing 3 files".into()));
        assert_eq!(update.len(), 1);
        assert_eq!(update[0]["params"]["value"]["kind"], "report");
        assert_eq!(update[0]["params"]["value"]["message"], "Analyzing 3 files");

        let end = reporter.report(ServerStatus::Ready);
        assert_eq!(end.len(), 1);
        assert_eq!(end[0]["params"]["value"]["kind"], "end");
        assert!(reporter.report(ServerStatus::Ready).is_empty());
    }

    #[test]
    fn ignores_status_without_client_support() {
        let mut reporter = StatusReporter::default();
        assert!(reporter
            .report(ServerStatus::Working("Loading project".into()))
            .is_empty());
        assert!(reporter.report(ServerStatus::Ready).is_empty());
    }
}
