use serde_json::{Value, json};
use std::collections::{HashSet, VecDeque};

#[derive(Default)]
pub(crate) struct NetworkLog {
    pub(crate) entries: VecDeque<Value>,
    pending: HashSet<String>,
    last_activity: f64,
}

impl NetworkLog {
    pub(crate) fn activity(&self) -> (usize, f64) {
        (self.pending.len(), self.last_activity)
    }

    pub(crate) fn reset_session(&mut self) {
        self.pending.clear();
        self.last_activity = 0.0;
    }

    fn entry_mut(&mut self, id: &str) -> Option<&mut Value> {
        self.entries
            .iter_mut()
            .rev()
            .find(|entry| entry["id"] == id)
    }

    pub(crate) fn request(&mut self, event: &Value, time: f64, log_cap: usize) {
        let Some(id) = event["requestId"].as_str() else {
            return;
        };
        let request = &event["request"];
        // CDP reuses the request ID through every hop of a redirect chain.
        if let Some(redirect) = event.get("redirectResponse")
            && let Some(entry) = self.entry_mut(id)
        {
            entry["status"] = redirect["status"].clone();
            entry["mime"] = redirect["mimeType"].clone();
        }
        if self.entries.len() >= log_cap {
            self.entries.pop_front();
        }
        self.entries.push_back(json!({
            "ts": time, "id": id, "method": request["method"], "url": request["url"], "type": event["type"], "status": null,
        }));

        // Extension-injected resources can appear here without a terminal event.
        // The document ready-state check still covers resources needed for page load.
        if request["url"]
            .as_str()
            .is_some_and(|url| url.starts_with("chrome-extension://"))
        {
            self.finish_pending(id, time);
        } else {
            self.pending.insert(id.to_owned());
            self.last_activity = time;
        }
    }

    pub(crate) fn response(&mut self, event: &Value, time: f64) {
        let Some(id) = event["requestId"].as_str() else {
            return;
        };
        let response = &event["response"];
        if let Some(entry) = self.entry_mut(id) {
            entry["status"] = response["status"].clone();
            entry["mime"] = response["mimeType"].clone();
        }
        // Wait for the stream handshake. Its body can stay open for the life of the page.
        if response["mimeType"]
            .as_str()
            .is_some_and(|mime| mime.eq_ignore_ascii_case("text/event-stream"))
        {
            self.finish_pending(id, time);
        }
    }

    pub(crate) fn finished(&mut self, event: &Value, time: f64) {
        let Some(id) = event["requestId"].as_str() else {
            return;
        };
        if let Some(error) = event.get("errorText")
            && let Some(entry) = self.entry_mut(id)
        {
            entry["failed"] = error.clone();
        }
        self.finish_pending(id, time);
    }

    fn finish_pending(&mut self, id: &str, time: f64) {
        if self.pending.remove(id) {
            self.last_activity = time;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_script_without_a_terminal_event_does_not_hold_the_page_open() {
        let mut log = NetworkLog::default();
        log.request(
            &json!({"requestId": "1", "request": {"url": "http://127.0.0.1/page"}}),
            1.0,
            10,
        );
        log.response(&json!({"requestId": "1", "response": {"status": 200}}), 2.0);
        log.finished(&json!({"requestId": "1"}), 3.0);
        log.request(&json!({"requestId": "2", "type": "Script", "request": {"url": "chrome-extension://fixture/content.js"}}), 4.0, 10);

        assert_eq!(log.activity(), (0, 3.0));
        assert_eq!(log.entries.len(), 2);
        assert_eq!(log.entries[1]["id"], "2");
    }

    #[test]
    fn unmatched_completion_leaves_other_requests_pending() {
        let mut log = NetworkLog::default();
        log.request(
            &json!({"requestId": "1", "request": {"url": "http://127.0.0.1/slow"}}),
            1.0,
            10,
        );
        log.finished(&json!({"requestId": "old"}), 2.0);
        assert_eq!(log.activity(), (1, 1.0));

        log.finished(
            &json!({"requestId": "1", "errorText": "net::ERR_ABORTED"}),
            3.0,
        );
        log.finished(&json!({"requestId": "1"}), 4.0);
        assert_eq!(log.activity(), (0, 3.0));
        assert_eq!(log.entries[0]["failed"], "net::ERR_ABORTED");
    }

    #[test]
    fn event_stream_waits_for_headers_and_reset_keeps_the_log() {
        let mut log = NetworkLog::default();
        log.request(
            &json!({"requestId": "1", "request": {"url": "http://127.0.0.1/events"}}),
            1.0,
            10,
        );
        assert_eq!(log.activity(), (1, 1.0));

        log.response(&json!({"requestId": "1", "response": {"status": 200, "mimeType": "text/event-stream"}}), 2.0);
        assert_eq!(log.activity(), (0, 2.0));

        log.reset_session();
        assert_eq!(log.activity(), (0, 0.0));
        assert_eq!(log.entries[0]["status"], 200);
    }
}
