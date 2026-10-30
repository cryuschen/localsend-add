// Modified by localsend-add contributors, 2026. See NOTICE and LICENSE.
//! The receiving side: incoming transfer requests, the Y/N/P decision and the
//! active receive session.

use super::App;
use super::progress::{Phase, Transfer};
use super::web_link::WebMode;
use crate::sanitize;
use crate::ui::Category;
use crate::util::{self, SpeedMeter};
use localsend::http::client::v2::LsHttpClientV2;
use localsend::http::server::common::save::FileUploadTarget;
use localsend::http::server::v2::{PrepareUploadDecisionV2, ServerEventV2, SessionEndReasonV2};
use localsend::model::discovery::ProtocolType;
use localsend::model::transfer::FileDto;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

/// Where to reach the device a receive session originates from, so a
/// receiver-side cancel can be delivered back to it over HTTP.
#[derive(Clone)]
pub(super) struct SenderTarget {
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) protocol: ProtocolType,
    pub(super) fingerprint: String,
}

/// An incoming transfer request waiting for the user's Y/N/P decision.
pub(super) struct PendingReceive {
    pub(super) session_id: String,
    pub(super) alias: String,
    pub(super) sender: SenderTarget,
    pub(super) files: HashMap<String, FileDto>,
    pub(super) decision_tx: oneshot::Sender<PrepareUploadDecisionV2>,
    pub(super) deadline: Instant,
}

const REQUEST_LIFETIME: Duration = Duration::from_secs(4);

fn expire_request(pending: &mut Option<PendingReceive>, now: Instant) -> Option<String> {
    if !pending
        .as_ref()
        .is_some_and(|request| now >= request.deadline)
    {
        return None;
    }
    let request = pending.take().unwrap();
    let _ = request.decision_tx.send(PrepareUploadDecisionV2::Decline);
    Some(request.alias)
}

/// An accepted upload session that is being received.
pub(super) struct ReceiveSession {
    pub(super) session_id: String,
    pub(super) alias: String,
    pub(super) sender: SenderTarget,
    pub(super) files: HashMap<String, FileDto>,
    pub(super) total_bytes: u64,
    pub(super) finished_files: usize,
    pub(super) failed_files: usize,
    pub(super) finalized_bytes: u64,
    pub(super) in_progress: HashMap<String, Arc<AtomicU64>>,
    // Per-file high-water marks survive completed/failed attempts and retries.
    file_progress: HashMap<String, Arc<AtomicU64>>,
    failed_ids: HashSet<String>,
    pub(super) started: Instant,
    targets: HashMap<String, PathBuf>,
    file_order: Vec<String>,
    saved_paths: HashMap<String, PathBuf>,
    last_error: String,
    pub(super) speed: SpeedMeter,

    /// Set when the server reported the end of the session. The summary is
    /// deferred until the in-flight per-file results have all arrived.
    pub(super) ended: Option<SessionEndReasonV2>,
}

impl ReceiveSession {
    fn new(
        session_id: String,
        alias: String,
        sender: SenderTarget,
        files: HashMap<String, FileDto>,
    ) -> Self {
        let total_bytes = files.values().map(|file| file.size).sum();
        Self {
            session_id,
            alias,
            sender,
            files,
            total_bytes,
            finished_files: 0,
            failed_files: 0,
            finalized_bytes: 0,
            in_progress: HashMap::new(),
            file_progress: HashMap::new(),
            failed_ids: HashSet::new(),
            started: Instant::now(),
            targets: HashMap::new(),
            file_order: Vec::new(),
            saved_paths: HashMap::new(),
            last_error: String::new(),
            speed: SpeedMeter::new(),
            ended: None,
        }
    }

    pub(super) fn progress(&self) -> Transfer {
        let mut transfer = Transfer::running(
            self.session_id.clone(),
            self.alias.clone(),
            self.done_bytes(),
            self.total_bytes,
        );
        // Protocol metadata is an unordered map. Use upload arrival order,
        // independent of filename sorting, completion order or renamed paths.
        transfer.paths = self
            .file_order
            .iter()
            .filter_map(|id| self.saved_paths.get(id).cloned())
            .collect();
        transfer.detail = self.last_error.clone();
        transfer
    }

    fn remember_target(&mut self, file_id: &str, path: PathBuf) {
        if !self.file_order.iter().any(|id| id == file_id) {
            self.file_order.push(file_id.to_string());
        }
        self.targets.insert(file_id.to_string(), path);
    }

    fn start_file_progress(&mut self, file_id: &str) -> Arc<AtomicU64> {
        let progress = self
            .file_progress
            .entry(file_id.to_string())
            .or_insert_with(|| Arc::new(AtomicU64::new(0)))
            .clone();
        self.in_progress
            .insert(file_id.to_string(), progress.clone());
        progress
    }

    fn file_saved(&mut self, file_id: &str) {
        let size = self.files.get(file_id).map(|file| file.size).unwrap_or(0);
        if !self.saved_paths.contains_key(file_id) {
            self.finished_files += 1;
            self.finalized_bytes += size;
        }
        if let Some(path) = self.targets.remove(file_id) {
            self.saved_paths.insert(file_id.to_string(), path);
        }
        self.failed_ids.remove(file_id);
        self.failed_files = self.failed_ids.len();
        self.file_progress
            .entry(file_id.to_string())
            .or_insert_with(|| Arc::new(AtomicU64::new(0)))
            .fetch_max(size, Ordering::Relaxed);
    }

    fn file_result(&mut self, file_id: &str, result: &Result<(), String>) {
        self.in_progress.remove(file_id);
        match result {
            Ok(()) => self.file_saved(file_id),
            Err(error) => {
                if !self.saved_paths.contains_key(file_id) {
                    self.failed_ids.insert(file_id.to_string());
                }
                self.failed_files = self.failed_ids.len();
                self.targets.remove(file_id);
                self.last_error = error.clone();
            }
        }
    }

    fn outcome(&self) -> Phase {
        if self.failed_files > 0 {
            Phase::Failed
        } else if matches!(self.ended, Some(SessionEndReasonV2::Cancelled)) {
            Phase::Cancelled
        } else if self.finished_files == self.files.len() {
            Phase::Success
        } else {
            Phase::Failed
        }
    }

    pub(super) fn done_bytes(&self) -> u64 {
        self.files
            .iter()
            .map(|(id, file)| {
                self.file_progress
                    .get(id)
                    .map(|progress| progress.load(Ordering::Relaxed).min(file.size))
                    .unwrap_or(0)
            })
            .sum()
    }
}

/// Returns the text of a "message request": a single text file whose content
/// is already included as a preview, so there is nothing left to transfer.
fn message_of(files: &HashMap<String, FileDto>) -> Option<&str> {
    if files.len() != 1 {
        return None;
    }
    let file = files.values().next()?;
    if file.file_type != "text" && !file.file_type.starts_with("text/") {
        return None;
    }
    file.preview.as_deref()
}

/// Store inline messages as plain UTF-8, with an atomic, non-overwriting
/// filename and a .txt suffix so the system text editor can open them.
async fn save_message(destination: &std::path::Path, message: &str) -> Result<PathBuf, String> {
    use tokio::io::AsyncWriteExt;
    let (path, mut file) = loop {
        let path = util::unique_path(destination, "LocalSend-文本.txt");
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&path).await {
            Ok(file) => break (path, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Failed to save message: {error}")),
        }
    };
    let result = async {
        file.write_all(message.as_bytes()).await?;
        file.flush().await
    }
    .await;
    drop(file);
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(format!("Failed to save message: {error}"));
    }
    Ok(path)
}

/// Maximum number of files listed in an incoming request prompt.
const MAX_LISTED_FILES: usize = 20;

/// The user's decision on a [`PendingReceive`].
#[derive(Debug, PartialEq)]
pub(super) enum Answer {
    Accept,
    Decline,
    AcceptAndPair,
}

impl App {
    pub(super) fn handle_server_event(&mut self, event: ServerEventV2) {
        match event {
            ServerEventV2::Register { ip, info } => {
                self.device_confirmed(ip.to_string(), info);
            }
            ServerEventV2::PrepareUpload {
                session_id,
                ip,
                info,
                cert_fingerprint,
                files,
                decision_tx,
            } => {
                // The sender is clearly reachable; make sure it has a slot.
                self.device_confirmed(ip.to_string(), info.clone());

                let alias = sanitize::single_line(&info.alias);
                // Do not reveal an older result after declining a newer request.
                self.last_receive = None;
                let sender = SenderTarget {
                    host: ip.to_string(),
                    port: info.port,
                    protocol: info.protocol,
                    fingerprint: cert_fingerprint.unwrap_or_else(|| info.fingerprint.clone()),
                };

                if self.storage.paired.contains(&sender.fingerprint) {
                    self.accept_receive(PendingReceive {
                        session_id,
                        alias,
                        sender,
                        files,
                        decision_tx,
                        deadline: Instant::now() + REQUEST_LIFETIME,
                    });
                } else {
                    let total: u64 = files.values().map(|file| file.size).sum();
                    let mut lines = vec![
                        alias.clone(),
                        format!("\nFiles ({}, {}):", files.len(), util::format_bytes(total)),
                    ];
                    let mut sorted: Vec<&FileDto> = files.values().collect();
                    sorted.sort_by_key(|file| std::cmp::Reverse(file.size));
                    for file in sorted.iter().take(MAX_LISTED_FILES) {
                        lines.push(format!(
                            "  {} ({})",
                            sanitize::single_line(&file.file_name),
                            util::format_bytes(file.size)
                        ));
                    }
                    if sorted.len() > MAX_LISTED_FILES {
                        lines.push(format!(
                            "  ... and {} more",
                            sorted.len() - MAX_LISTED_FILES
                        ));
                    }
                    // Web senders have no stable identity worth pairing: the
                    // fingerprint is an unverified payload value.
                    let prompt = match self.web {
                        Some(WebMode::Receive) => "\nAccept? Y/N",
                        _ => "\nAccept? Y/N/P (P = accept and pair)",
                    };
                    lines.push(prompt.to_string());
                    self.ui.log(Category::Receive, &lines.join("\n"));
                    self.pending = Some(PendingReceive {
                        session_id,
                        alias,
                        sender,
                        files,
                        decision_tx,
                        deadline: Instant::now() + REQUEST_LIFETIME,
                    });
                }
            }
            ServerEventV2::FileUpload {
                session_id,
                file_id,
                file,
                target_tx,
            } => self.handle_file_upload(session_id, file_id, file, target_tx),
            ServerEventV2::SessionEnd { session_id, reason } => {
                let Some(session) = self
                    .receive
                    .as_mut()
                    .filter(|session| session.session_id == session_id)
                else {
                    return;
                };
                // The per-file results race with this event (they arrive on
                // the app's own channel); print the summary only once the
                // last in-flight file has reported its outcome.
                session.ended = Some(reason);
                self.finish_receive_if_done();
            }
            ServerEventV2::PrepareUploadAborted { session_id } => {
                if let Some(pending) = &self.pending
                    && pending.session_id == session_id
                {
                    let pending = self.pending.take().unwrap();
                    self.ui.log(
                        Category::Receive,
                        &format!("{}: Aborted by sender", pending.alias),
                    );
                }
            }
            ServerEventV2::CancelReceived { ip, session_id } => {
                if let Some(send) = &self.send
                    && send.session_id.as_deref() == Some(session_id.as_str())
                    && send.host == ip.to_string()
                {
                    send.cancel.by_peer.store(true, Ordering::Relaxed);
                    send.cancel.token.cancel();
                }
            }
            ServerEventV2::ListenerFailed { error } => {
                self.ui.log(
                    Category::Receive,
                    &format!("Server stopped, no longer receiving: {error}"),
                );
            }
        }
    }

    fn handle_file_upload(
        &mut self,
        session_id: String,
        file_id: String,
        file: FileDto,
        target_tx: oneshot::Sender<FileUploadTarget>,
    ) {
        let Some(session) = self
            .receive
            .as_mut()
            .filter(|session| session.session_id == session_id)
        else {
            // Unknown session: dropping the responder fails the request.
            return;
        };

        let reserved: Vec<_> = session
            .targets
            .iter()
            .filter(|(id, _)| *id != &file_id)
            .map(|(_, path)| path.clone())
            .collect();
        let path =
            util::unique_path_reserved(&self.storage.destination, &file.file_name, &reserved);
        session.remember_target(&file_id, path.clone());

        let progress = session.start_file_progress(&file_id);

        let (progress_tx, mut progress_rx) = mpsc::channel::<u64>(16);
        let progress_task = tokio::spawn(async move {
            while let Some(written) = progress_rx.recv().await {
                progress.fetch_max(written, Ordering::Relaxed);
            }
        });

        let (result_tx, result_rx) = oneshot::channel::<Result<(), String>>();
        tokio::spawn({
            let events_tx = self.events_tx.clone();
            async move {
                let result = match result_rx.await {
                    Ok(result) => result,
                    Err(_) => Err("Upload aborted".to_string()),
                };
                // The writer has closed its progress sender. Drain those final
                // reports before publishing a terminal snapshot for this file.
                let _ = progress_task.await;
                let _ = events_tx
                    .send(super::AppEvent::ReceiveFileResult {
                        session_id,
                        file_id,
                        result,
                    })
                    .await;
            }
        });

        let _ = target_tx.send(FileUploadTarget::AtomicPath {
            path,
            result_tx,
            progress_tx: Some(progress_tx),
        });
    }

    pub(super) fn receive_file_result(
        &mut self,
        session_id: String,
        file_id: String,
        result: Result<(), String>,
    ) {
        let Some(session) = self
            .receive
            .as_mut()
            .filter(|session| session.session_id == session_id)
        else {
            return;
        };
        session.file_result(&file_id, &result);
        if let Err(err) = result {
            let name = session
                .files
                .get(&file_id)
                .map(|file| sanitize::single_line(&file.file_name))
                .unwrap_or_else(|| sanitize::single_line(&file_id));
            let alias = session.alias.clone();
            self.ui.log(
                Category::Receive,
                &format!("{alias}: Failed to receive {name}: {err}"),
            );
        }
        self.finish_receive_if_done();
    }

    /// Prints the session summary and clears the session once the server
    /// reported its end and no per-file result is outstanding.
    fn finish_receive_if_done(&mut self) {
        let done = self
            .receive
            .as_ref()
            .is_some_and(|session| session.ended.is_some() && session.in_progress.is_empty());
        if !done {
            return;
        }
        let session = self.receive.take().unwrap();
        let mut transfer = session.progress();
        transfer.finish(session.outcome());
        self.last_receive = Some(transfer);
        match session.ended.unwrap() {
            SessionEndReasonV2::Finished => {
                let mut text = format!(
                    "{}: Received {} file{} ({}, took {})",
                    session.alias,
                    session.finished_files,
                    if session.finished_files == 1 { "" } else { "s" },
                    util::format_bytes(session.finalized_bytes),
                    util::format_duration(session.started.elapsed()),
                );
                if session.failed_files > 0 {
                    text.push_str(&format!(", {} failed", session.failed_files));
                }
                self.ui.log(Category::Receive, &text);
            }
            SessionEndReasonV2::Cancelled => {
                self.ui.log(
                    Category::Receive,
                    &format!(
                        "{}: Cancelled by sender ({} of {} files received)",
                        session.alias,
                        session.finished_files,
                        session.files.len(),
                    ),
                );
            }
        }
        self.render_status();
    }

    /// Cancels the active receive session: rejects further uploads on the
    /// server, notifies the sender and prints the summary. Late per-file
    /// results are dropped by the session-id filters.
    pub(super) fn cancel_receive(&mut self) {
        let Some(session) = self.receive.take() else {
            return;
        };
        let mut transfer = session.progress();
        transfer.finish(Phase::Cancelled);
        self.last_receive = Some(transfer);
        let server = self.server.clone();
        let identity = self.storage.identity.clone();
        let sender = session.sender.clone();
        let session_id = session.session_id.clone();
        tokio::spawn(async move {
            server.cancel_v2_session(&session_id).await;

            // Best effort: without it the sender only notices through its
            // failing upload requests.
            let expected_fingerprint = match sender.protocol {
                ProtocolType::Https => Some(sender.fingerprint.clone()),
                ProtocolType::Http => None,
            };
            let Ok(client) = LsHttpClientV2::try_new(
                &identity.key_pem,
                &identity.cert_pem,
                expected_fingerprint,
                Some(Duration::from_secs(5)),
            ) else {
                return;
            };
            let protocol = match sender.protocol {
                ProtocolType::Http => ProtocolType::Http,
                ProtocolType::Https => ProtocolType::Https,
            };
            let _ = client
                .cancel(protocol, &sender.host, sender.port, &session_id)
                .await;
        });
        self.ui.log(
            Category::Receive,
            &format!(
                "{}: cancelled ({} of {} files received)",
                session.alias,
                session.finished_files,
                session.files.len(),
            ),
        );
        self.render_status();
    }

    fn accept_receive(&mut self, mut pending: PendingReceive) {
        if let Some(message) = message_of(&pending.files).map(str::to_owned) {
            let file_id = pending.files.keys().next().unwrap().clone();
            // The preview contains the complete UTF-8 message. Count its actual
            // bytes, independent of the sender's declared file size or name.
            pending.files.get_mut(&file_id).unwrap().size = message.len() as u64;
            let mut session = ReceiveSession::new(
                pending.session_id.clone(),
                pending.alias,
                pending.sender,
                pending.files,
            );
            session.start_file_progress(&file_id);
            self.receive = Some(session);
            let destination = self.storage.destination.clone();
            let events = self.events_tx.clone();
            tokio::spawn(async move {
                let result = save_message(&destination, &message).await;
                let _ = events
                    .send(super::AppEvent::ReceiveMessageResult {
                        session_id: pending.session_id,
                        file_id,
                        decision_tx: pending.decision_tx,
                        result,
                    })
                    .await;
            });
            return;
        }
        let ids = pending.files.keys().cloned().collect();
        if pending
            .decision_tx
            .send(PrepareUploadDecisionV2::Accept(ids))
            .is_err()
        {
            self.ui.log(
                Category::Receive,
                &format!("{}: request already ended", pending.alias),
            );
            return;
        }
        self.receive = Some(ReceiveSession::new(
            pending.session_id,
            pending.alias,
            pending.sender,
            pending.files,
        ));
    }

    pub(super) fn receive_message_result(
        &mut self,
        session_id: String,
        file_id: String,
        decision_tx: oneshot::Sender<PrepareUploadDecisionV2>,
        result: Result<PathBuf, String>,
    ) {
        let active = self
            .receive
            .as_ref()
            .is_some_and(|session| session.session_id == session_id);
        if !active || decision_tx.is_closed() {
            let _ = decision_tx.send(PrepareUploadDecisionV2::Decline);
            if let Ok(path) = result {
                tokio::spawn(async move {
                    let _ = tokio::fs::remove_file(path).await;
                });
            }
            if active {
                let session = self.receive.take().unwrap();
                let mut transfer = session.progress();
                transfer.detail = "Message request cancelled by sender".into();
                transfer.finish(Phase::Cancelled);
                self.last_receive = Some(transfer);
            }
            return;
        }
        let session = self.receive.as_mut().unwrap();
        let saved = result.is_ok();
        let outcome = result.map(|path| session.remember_target(&file_id, path));
        session.file_result(&file_id, &outcome);
        session.ended = Some(SessionEndReasonV2::Finished);
        // Inline messages end with 204: never ask the sender to upload again.
        let response = if saved {
            PrepareUploadDecisionV2::Accept(HashSet::new())
        } else {
            PrepareUploadDecisionV2::Decline
        };
        let _ = decision_tx.send(response);
        self.finish_receive_if_done();
    }

    pub(super) fn expire_pending(&mut self, now: Instant) {
        if let Some(alias) = expire_request(&mut self.pending, now) {
            self.ui.log(
                Category::Receive,
                &format!("{alias}: No answer within 4 seconds; declined"),
            );
        }
    }

    pub(super) fn answer_pending(&mut self, answer: Answer) {
        self.expire_pending(Instant::now());
        let Some(pending) = self.pending.take() else {
            return;
        };
        match answer {
            Answer::Decline => {
                let _ = pending.decision_tx.send(PrepareUploadDecisionV2::Decline);
                self.ui.log(
                    Category::Receive,
                    &format!("{}: You declined", pending.alias),
                );
            }
            Answer::Accept | Answer::AcceptAndPair => {
                self.ui.log(
                    Category::Receive,
                    &format!("{}: You accepted", pending.alias),
                );
                // While "receive via link" is on, P was not offered; a stray
                // press still accepts, but never pairs.
                if matches!(answer, Answer::AcceptAndPair)
                    && !matches!(self.web, Some(WebMode::Receive))
                {
                    let channels = self
                        .discovery
                        .device_by_fingerprint(&pending.sender.fingerprint)
                        .map(|stored| crate::storage::PairedChannel::channels_of(&stored))
                        .unwrap_or_default();
                    match self.storage.paired.insert(
                        pending.sender.fingerprint.clone(),
                        pending.alias.clone(),
                        channels,
                    ) {
                        Ok(()) => self.ui.log(
                            Category::Receive,
                            &format!(
                                "{}: Paired. Future requests are auto-accepted.",
                                pending.alias
                            ),
                        ),
                        Err(err) => self.ui.log(
                            Category::Receive,
                            &format!(
                                "{}: Could not save pairing; accepted once: {err:#}",
                                pending.alias
                            ),
                        ),
                    }
                }
                self.accept_receive(pending);
            }
        }
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use serde_json::json;

    fn batch() -> ReceiveSession {
        let files = [
            ("first", "z-last-alphabetically.txt", 40),
            ("second", "a-first-alphabetically.txt", 60),
        ]
        .into_iter()
        .map(|(id, name, size)| {
            let file: FileDto = serde_json::from_value(
                json!({"id":id,"fileName":name,"size":size,"fileType":"application/octet-stream"}),
            )
            .unwrap();
            (id.to_string(), file)
        })
        .collect();
        ReceiveSession::new(
            "one-batch".into(),
            "phone".into(),
            SenderTarget {
                host: "127.0.0.1".into(),
                port: 53317,
                protocol: ProtocolType::Https,
                fingerprint: "TEST".into(),
            },
            files,
        )
    }

    #[tokio::test]
    async fn text_messages_preserve_utf8_and_do_not_overwrite_existing_files() {
        let dir = std::env::temp_dir().join(format!("localsend-message-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir(&dir).await.unwrap();
        let message = "中文文本\n第二行 <tag> & 空格  \r\n🙂";
        let first = save_message(&dir, message).await.unwrap();
        let second = save_message(&dir, "另一个消息").await.unwrap();
        assert_ne!(first, second);
        assert_eq!(first.extension().unwrap(), "txt");
        assert_eq!(tokio::fs::read(&first).await.unwrap(), message.as_bytes());
        assert_eq!(
            tokio::fs::read_to_string(&second).await.unwrap(),
            "另一个消息"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                tokio::fs::metadata(&first)
                    .await
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        // Inline messages use the same saved-path / success snapshot as files.
        let mut session = batch();
        session.files.remove("second");
        session.files.get_mut("first").unwrap().size = message.len() as u64;
        session.total_bytes = message.len() as u64;
        session.remember_target("first", first.clone());
        session.file_result("first", &Ok(()));
        let mut transfer = session.progress();
        transfer.finish(session.outcome());
        let snapshot = transfer.snapshot(&dir).unwrap();
        assert_eq!(snapshot["phase"], "success");
        assert_eq!(snapshot["paths"][0], first.to_string_lossy().as_ref());
        assert_eq!(snapshot["bytes"], message.len());
        assert!(snapshot["remaining_ms"].as_u64().unwrap() <= 5000);
        tokio::fs::remove_dir_all(dir).await.unwrap();
    }

    #[tokio::test]
    async fn text_message_save_failure_is_reported_instead_of_success() {
        let missing =
            std::env::temp_dir().join(format!("localsend-missing-{}", uuid::Uuid::new_v4()));
        assert!(
            save_message(&missing, "消息")
                .await
                .unwrap_err()
                .contains("Failed to save message")
        );
        assert!(!missing.exists());
    }

    #[test]
    fn only_a_single_inline_text_preview_is_a_message() {
        let mut session = batch();
        assert!(message_of(&session.files).is_none());
        session.files.remove("second");
        let file = session.files.get_mut("first").unwrap();
        file.file_type = "text/plain".into();
        assert!(message_of(&session.files).is_none());
        session.files.get_mut("first").unwrap().preview = Some("正文\n下一行".into());
        assert_eq!(message_of(&session.files), Some("正文\n下一行"));
        session.files.get_mut("first").unwrap().file_type = "image/png".into();
        assert!(message_of(&session.files).is_none());
    }

    fn request_at(
        start: Instant,
    ) -> (
        Option<PendingReceive>,
        oneshot::Receiver<PrepareUploadDecisionV2>,
    ) {
        let session = batch();
        let (decision_tx, decision_rx) = oneshot::channel();
        (
            Some(PendingReceive {
                session_id: session.session_id,
                alias: session.alias,
                sender: session.sender,
                files: session.files,
                decision_tx,
                deadline: start + REQUEST_LIFETIME,
            }),
            decision_rx,
        )
    }

    #[test]
    fn unanswered_request_is_declined_at_four_seconds_and_removed_immediately() {
        let start = Instant::now();
        let (mut pending, mut decision) = request_at(start);
        assert_eq!(REQUEST_LIFETIME, Duration::from_secs(4));
        for millis in [0, 1000, 3999] {
            assert!(expire_request(&mut pending, start + Duration::from_millis(millis)).is_none());
            assert!(pending.is_some());
            assert!(matches!(
                decision.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
        }
        assert_eq!(
            expire_request(&mut pending, start + REQUEST_LIFETIME).as_deref(),
            Some("phone")
        );
        assert!(pending.is_none());
        assert!(matches!(
            decision.try_recv(),
            Ok(PrepareUploadDecisionV2::Decline)
        ));
        assert!(expire_request(&mut pending, start + Duration::from_secs(8)).is_none());
    }

    #[test]
    fn answered_request_and_new_request_do_not_inherit_an_old_timeout() {
        let start = Instant::now();
        let (mut pending, mut decision) = request_at(start);
        let request = pending.take().unwrap();
        request
            .decision_tx
            .send(PrepareUploadDecisionV2::Accept(HashSet::new()))
            .unwrap();
        assert!(expire_request(&mut pending, start + REQUEST_LIFETIME).is_none());
        assert!(matches!(
            decision.try_recv(),
            Ok(PrepareUploadDecisionV2::Accept(_))
        ));
        let (mut pending, mut decision) = request_at(start + Duration::from_secs(3));
        assert!(expire_request(&mut pending, start + REQUEST_LIFETIME).is_none());
        assert!(pending.is_some());
        assert!(expire_request(&mut pending, start + Duration::from_secs(7)).is_some());
        assert!(matches!(
            decision.try_recv(),
            Ok(PrepareUploadDecisionV2::Decline)
        ));
    }

    #[test]
    fn opens_first_started_file_even_if_another_finishes_first_and_sorts_first() {
        let mut session = batch();
        let first = PathBuf::from("/tmp/z-last-alphabetically (1).txt");
        let second = PathBuf::from("/tmp/a-first-alphabetically.txt");
        session.remember_target("first", first.clone());
        session.remember_target("second", second.clone());
        session.file_saved("second");
        session.file_saved("first");
        assert_eq!(session.progress().paths, vec![first, second]);
        assert_eq!(session.progress().total_bytes, 100);
        assert_eq!(session.progress().bytes, 100);
        assert_eq!(session.outcome(), Phase::Success);
    }

    #[test]
    fn batch_aggregates_progress_and_a_retry_does_not_move_the_first_file() {
        let mut session = batch();
        session.remember_target("first", PathBuf::from("/tmp/z.txt"));
        session.remember_target("second", PathBuf::from("/tmp/a.txt"));
        session
            .start_file_progress("first")
            .fetch_max(20, Ordering::Relaxed);
        session
            .start_file_progress("second")
            .fetch_max(30, Ordering::Relaxed);
        let snapshot = session
            .progress()
            .snapshot(std::path::Path::new("/tmp"))
            .unwrap();
        assert_eq!(snapshot["id"], "one-batch");
        assert_eq!(snapshot["bytes"], 50);
        assert_eq!(snapshot["total_bytes"], 100);
        session.remember_target("first", PathBuf::from("/tmp/z (2).txt"));
        session.file_saved("second");
        session.file_saved("first");
        assert_eq!(
            session.progress().paths,
            vec![PathBuf::from("/tmp/z (2).txt"), PathBuf::from("/tmp/a.txt")]
        );
    }

    #[test]
    fn a_single_file_uses_the_same_batch_snapshot() {
        let mut session = batch();
        session.files.remove("second");
        session.total_bytes = 40;
        session.remember_target("first", PathBuf::from("/tmp/z.txt"));
        session.file_saved("first");
        assert_eq!(session.progress().paths, vec![PathBuf::from("/tmp/z.txt")]);
        assert_eq!(session.progress().bytes, session.progress().total_bytes);
    }
    #[test]
    fn failed_file_preserves_partial_bytes_after_active_counter_is_removed() {
        let mut session = batch();
        session.remember_target("first", PathBuf::from("/tmp/z.txt"));
        session.file_result("first", &Ok(()));
        session
            .start_file_progress("second")
            .fetch_max(30, Ordering::Relaxed);
        assert_eq!(session.done_bytes(), 70);
        session.file_result("second", &Err("connection lost".into()));
        assert!(session.in_progress.is_empty());
        assert_eq!(session.done_bytes(), 70);
        let mut transfer = session.progress();
        transfer.finish(Phase::Failed);
        let value = transfer.snapshot(std::path::Path::new("/tmp")).unwrap();
        assert_eq!(value["bytes"], 70);
        assert_eq!(value["phase"], "failed");
    }

    #[test]
    fn retry_keeps_high_water_mark_without_double_counting() {
        let mut session = batch();
        session.remember_target("first", PathBuf::from("/tmp/z.txt"));
        session.file_result("first", &Ok(()));
        session
            .start_file_progress("second")
            .fetch_max(30, Ordering::Relaxed);
        session.file_result("second", &Err("first attempt".into()));
        session
            .start_file_progress("second")
            .fetch_max(10, Ordering::Relaxed);
        assert_eq!(session.done_bytes(), 70);
        session
            .start_file_progress("second")
            .fetch_max(45, Ordering::Relaxed);
        assert_eq!(session.done_bytes(), 85);
        session.file_result("second", &Err("second attempt".into()));
        assert_eq!(session.failed_files, 1);
        session.remember_target("second", PathBuf::from("/tmp/a (1).txt"));
        session.file_result("second", &Ok(()));
        session.file_result("second", &Ok(()));
        assert_eq!(session.done_bytes(), 100);
        assert_eq!(session.finalized_bytes, 100);
        assert_eq!(session.finished_files, 2);
        assert_eq!(session.failed_files, 0);
        assert_eq!(session.outcome(), Phase::Success);
    }

    #[test]
    fn cancelled_batch_and_zero_progress_failure_do_not_fill_the_bar() {
        let mut session = batch();
        session
            .start_file_progress("first")
            .fetch_max(15, Ordering::Relaxed);
        session
            .start_file_progress("second")
            .fetch_max(20, Ordering::Relaxed);
        session.file_result("first", &Err("cancelled".into()));
        session.file_result("second", &Err("cancelled".into()));
        let mut transfer = session.progress();
        transfer.finish(Phase::Cancelled);
        assert_eq!(
            transfer.snapshot(std::path::Path::new("/tmp")).unwrap()["bytes"],
            35
        );
        let mut empty = batch().progress();
        empty.finish(Phase::Failed);
        assert_eq!(
            empty.snapshot(std::path::Path::new("/tmp")).unwrap()["bytes"],
            0
        );
    }
}
