//! Local panel snapshots. Reading these never starts discovery or networking.
use serde::Serialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(super) const RESULT_LIFETIME: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Running,
    Success,
    Failed,
    Cancelled,
}

#[derive(Clone)]
pub(super) struct Transfer {
    pub id: String,
    pub alias: String,
    pub phase: Phase,
    pub bytes: u64,
    pub total_bytes: u64,
    pub paths: Vec<PathBuf>,
    pub detail: String,
    pub ended: Option<Instant>,
}

impl Transfer {
    pub fn running(id: String, alias: String, bytes: u64, total_bytes: u64) -> Self {
        Self {
            id,
            alias,
            phase: Phase::Running,
            bytes,
            total_bytes,
            paths: Vec::new(),
            detail: String::new(),
            ended: None,
        }
    }

    pub fn finish(&mut self, phase: Phase) {
        self.phase = phase;
        if phase == Phase::Success {
            self.bytes = self.total_bytes;
        }
        self.ended = Some(Instant::now());
    }

    pub fn snapshot(&self, destination: &Path) -> Option<Value> {
        self.snapshot_at(destination, Instant::now())
    }

    fn snapshot_at(&self, destination: &Path, now: Instant) -> Option<Value> {
        let remaining = match self.ended {
            Some(at) => {
                let elapsed = now.saturating_duration_since(at);
                if elapsed >= RESULT_LIFETIME {
                    return None;
                }
                Some(RESULT_LIFETIME - elapsed)
            }
            None => None,
        };
        Some(
            json!({"id":self.id, "alias":crate::sanitize::single_line(&self.alias),
            "phase":self.phase, "bytes":self.bytes.min(self.total_bytes), "total_bytes":self.total_bytes,
            "paths":self.paths.iter().take(50).collect::<Vec<_>>(), "file_count":self.paths.len(),
            "destination":destination, "detail":crate::sanitize::single_line(&self.detail),
            "remaining_ms":remaining.map(|d|d.as_millis() as u64)}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completion_expires_from_its_original_time_not_the_poll_time() {
        let mut transfer = Transfer::running("1".into(), "phone".into(), 30, 100);
        assert!(
            transfer
                .snapshot_at(Path::new("/tmp"), Instant::now() + Duration::from_secs(60))
                .is_some()
        );
        transfer.finish(Phase::Success);
        let at = transfer.ended.unwrap();
        assert_eq!(
            transfer.snapshot_at(Path::new("/tmp"), at).unwrap()["bytes"],
            100
        );
        assert_eq!(
            transfer
                .snapshot_at(Path::new("/tmp"), at + Duration::from_secs(4))
                .unwrap()["remaining_ms"],
            1000
        );
        assert!(
            transfer
                .snapshot_at(Path::new("/tmp"), at + RESULT_LIFETIME)
                .is_none()
        );
    }
    #[test]
    fn failure_does_not_fill_progress_or_change_saved_paths() {
        let mut transfer = Transfer::running("2".into(), "phone\nname".into(), 30, 100);
        transfer.paths.push(PathBuf::from("/tmp/实际文件 (2).txt"));
        transfer.finish(Phase::Failed);
        let value = transfer.snapshot(Path::new("/tmp")).unwrap();
        assert_eq!(value["bytes"], 30);
        assert_eq!(value["phase"], "failed");
        assert_eq!(value["paths"][0], "/tmp/实际文件 (2).txt");
        assert_eq!(value["alias"], "phone name");
    }
}
