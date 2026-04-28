use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a tool must be idle before the next event is treated as a new session.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes

/// Tracks per-tool session windows using a simple last-seen timestamp.
pub struct SessionWindowMap {
    inner: Mutex<HashMap<String, Instant>>,
}

impl Default for SessionWindowMap {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionWindowMap {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Record activity for `tool`. Returns `true` if this starts a new session
    /// (tool was previously idle or unseen).
    pub fn touch(&self, tool: &str) -> bool {
        let mut map = self.inner.lock().unwrap();
        let now = Instant::now();
        let is_new = map
            .get(tool)
            .map(|last| now.duration_since(*last) >= IDLE_TIMEOUT)
            .unwrap_or(true);
        map.insert(tool.to_string(), now);
        is_new
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_touch_is_new_session() {
        let m = SessionWindowMap::new();
        assert!(m.touch("cursor"));
    }

    #[test]
    fn immediate_second_touch_is_same_session() {
        let m = SessionWindowMap::new();
        m.touch("cursor");
        assert!(!m.touch("cursor"));
    }

    #[test]
    fn different_tools_are_independent() {
        let m = SessionWindowMap::new();
        m.touch("cursor");
        assert!(
            m.touch("codex"),
            "codex is a new session even if cursor is active"
        );
    }
}
