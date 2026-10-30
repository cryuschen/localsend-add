// Modified by localsend-add contributors, 2026. See NOTICE and LICENSE.
use crate::http::dto_v2::{
    InfoResponseDtoV2, PrepareUploadRequestDtoV2, PrepareUploadResponseDtoV2, RegisterDtoV2,
    RegisterResponseDtoV2,
};
use crate::http::server::common::collect_to_json::CollectToJson;
use crate::http::server::common::error::AppError;
use crate::http::server::common::pin::check_pin;
use crate::http::server::common::query::parse_query;
use crate::http::server::common::response::{empty_body, BoxedBody, JsonResponse};
use crate::http::server::common::save::UPLOAD_IDLE_TIMEOUT;
use crate::http::server::common::save::{FileTimestamps, FileUploadTarget, SaveResult};
use crate::http::server::common::session::{
    FileStatusV2, PendingSessionV2, SessionFileV2, SessionStateV2, UploadSessionV2,
};
use crate::http::server::PeerIp;
use crate::http::server::{common, AppState, RequestClientInfo, V2State};
use crate::model::discovery::PROTOCOL_VERSION_V2;
use crate::model::transfer::FileDto;
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Events emitted by the v2 HTTP server that must be handled by the application.
#[derive(Debug)]
pub enum ServerEventV2 {
    /// A device registered itself via `POST /api/localsend/v2/register`.
    ///
    /// On TLS, this event is only emitted when `info.fingerprint` matches the
    /// SHA-256 fingerprint of the client certificate verified during the mTLS
    /// handshake, so the fingerprint cannot be spoofed.
    Register {
        /// The IP address of the remote device.
        ip: PeerIp,

        /// The device information sent by the remote device.
        info: RegisterDtoV2,
    },

    /// A sender requests to upload files via `POST /api/localsend/v2/prepare-upload`.
    ///
    /// The application must answer on `decision_tx`.
    /// Dropping `decision_tx` results in a 500 response.
    PrepareUpload {
        /// The session ID the upload session will have when the request is
        /// accepted. Pre-generated so the application can track the session
        /// consistently from the start.
        session_id: String,

        /// The IP address of the sender.
        ip: PeerIp,

        /// The device information of the sender.
        info: RegisterDtoV2,

        /// The SHA-256 fingerprint (uppercase hex) of the sender's client
        /// certificate verified during the mTLS handshake. Unlike
        /// `info.fingerprint`, this value cannot be spoofed.
        /// `None` when the server runs without TLS.
        cert_fingerprint: Option<String>,

        /// The offered files, mapped by file ID.
        files: HashMap<String, FileDto>,

        /// Channel to send the decision (accept all, a subset, or decline).
        decision_tx: oneshot::Sender<PrepareUploadDecisionV2>,
    },

    /// An accepted file is being uploaded via `POST /api/localsend/v2/upload`.
    ///
    /// The application must answer on `target_tx` with where the file content
    /// should go (a stream to consume itself, a path, or a file descriptor).
    /// Dropping `target_tx` results in a 500 response.
    FileUpload {
        /// The session ID of the upload session.
        session_id: String,

        /// The ID of the file being uploaded.
        file_id: String,

        /// The metadata of the file being uploaded.
        file: FileDto,

        /// Channel to send the target the file content should be written to.
        target_tx: oneshot::Sender<FileUploadTarget>,
    },

    /// An upload session ended.
    SessionEnd {
        /// The session ID of the ended session.
        session_id: String,

        /// Why the session ended.
        reason: SessionEndReasonV2,
    },

    /// A prepare-upload request was aborted before a session was created,
    /// e.g. the sender disconnected while the application was still deciding.
    /// The `decision_tx` of the [ServerEventV2::PrepareUpload] with the same
    /// session ID is dead; answering it has no effect.
    PrepareUploadAborted {
        /// The session ID of the aborted prepare-upload request.
        session_id: String,
    },

    /// `POST /api/localsend/v2/cancel` was received for a session this server
    /// does not manage. This happens when the remote device cancels a transfer
    /// that this application is currently *sending* to it: the session ID is
    /// the one issued by the remote device during prepare-upload.
    ///
    /// The application must verify that `ip` matches the target of the
    /// send session before cancelling it.
    CancelReceived {
        /// The IP address of the remote device requesting the cancellation.
        ip: PeerIp,

        /// The session ID as known by the remote device.
        session_id: String,
    },

    /// The listening socket failed permanently, e.g. because the OS
    /// invalidated it while the application was suspended (iOS reclaims the
    /// sockets of suspended apps). The server has stopped itself; the
    /// application must restart it to become reachable again.
    ListenerFailed {
        /// Description of the failure.
        error: String,
    },
}

/// The application's decision for a prepare-upload request.
#[derive(Debug)]
pub enum PrepareUploadDecisionV2 {
    /// Accept the given file IDs (a subset of the offered files).
    /// An empty set responds with 204 (no file transfer needed).
    Accept(HashSet<String>),

    /// Decline the request (403).
    Decline,
}

/// Why an upload session ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionEndReasonV2 {
    /// All accepted files reached a final state (finished or failed).
    Finished,

    /// The sender cancelled the session via `POST /api/localsend/v2/cancel`.
    Cancelled,
}

pub(crate) async fn register(
    body: Incoming,
    state: AppState,
    client_info: RequestClientInfo,
) -> Result<JsonResponse<RegisterResponseDtoV2>, AppError> {
    let payload = body.collect_to_json::<RegisterDtoV2>().await?;

    // On TLS, only trust registrations whose claimed fingerprint is proven
    // by the client certificate of the mTLS handshake.
    let fingerprint_valid = match client_info.cert_fingerprint() {
        Some(cert_fingerprint) => payload.fingerprint.to_ascii_uppercase() == cert_fingerprint,
        None => true,
    };

    if let Some(v2) = &state.v2 {
        if fingerprint_valid {
            // Not awaited: registrations arrive in bursts (every device on the
            // network answers an announcement, and a peer scanning its subnet
            // registers with everyone), so the channel fills up easily. Waiting
            // would block this request handler — and every later one — until
            // the application catches up, which is what makes the device stop
            // answering `register` altogether.
            //
            // The event carries no responder, and peers repeat their
            // announcement, so a dropped registration is recoverable.
            if let Err(err) = v2.event_tx.try_send(ServerEventV2::Register {
                ip: client_info.ip,
                info: payload,
            }) {
                tracing::debug!("Dropped a register event: {err}");
            }
        } else {
            tracing::warn!(
                "Ignoring register from {}: claimed fingerprint does not match the client certificate",
                client_info.ip
            );
        }
    }

    let info = state.info.lock().await.clone();
    let download = state.web.share.download().is_some();

    Ok(JsonResponse {
        status: StatusCode::OK,
        body: RegisterResponseDtoV2 {
            alias: info.alias,
            version: PROTOCOL_VERSION_V2.to_string(),
            device_model: info.device_model,
            device_type: info.device_type,
            fingerprint: info.token,
            download,
        },
    })
}

pub(crate) async fn info(state: AppState) -> Result<JsonResponse<InfoResponseDtoV2>, AppError> {
    let info = state.info.lock().await.clone();
    let download = state.web.share.download().is_some();

    Ok(JsonResponse {
        status: StatusCode::OK,
        body: InfoResponseDtoV2 {
            alias: info.alias,
            version: PROTOCOL_VERSION_V2.to_string(),
            device_model: info.device_model,
            device_type: info.device_type,
            fingerprint: info.token,
            download,
        },
    })
}

pub(crate) async fn prepare_upload(
    req: Request<Incoming>,
    state: AppState,
    client_info: RequestClientInfo,
) -> Result<Response<BoxedBody>, AppError> {
    let v2 = require_v2(&state)?;
    let query = parse_query(req.uri().query());

    check_pin(
        v2.pin.as_deref(),
        &v2.pin_attempts,
        &query,
        client_info.ip.ip,
    )
    .await?;

    let payload = req
        .into_body()
        .collect_to_json::<PrepareUploadRequestDtoV2>()
        .await?;

    if payload.files.is_empty() {
        return Err(AppError::BadRequest("No files provided".to_string()));
    }

    let session_id = Uuid::new_v4().to_string();
    let cancelled = CancellationToken::new();

    // Claim the single session slot.
    {
        let mut slot = v2.session.lock().await;
        if slot.is_some() {
            return Err(AppError::Message(
                StatusCode::CONFLICT,
                "Blocked by another session".to_string(),
            ));
        }
        *slot = Some(SessionStateV2::Pending(PendingSessionV2 {
            session_id: session_id.clone(),
            sender_ip: client_info.ip,
            cancel: cancelled.clone(),
        }));
    }

    // Frees the slot again if this request is aborted before a session is created.
    let mut pending_guard = PendingSessionGuard::new(v2.clone(), session_id.clone());

    let (decision_tx, decision_rx) = oneshot::channel();
    let event = ServerEventV2::PrepareUpload {
        session_id: session_id.clone(),
        ip: client_info.ip,
        info: payload.info,
        cert_fingerprint: client_info.cert_fingerprint(),
        files: payload.files.clone(),
        decision_tx,
    };
    if v2.event_tx.send(event).await.is_err() {
        return Err(AppError::Status(StatusCode::INTERNAL_SERVER_ERROR));
    }

    // The sender may cancel the request while the application is deciding.
    // Returning with the guard still armed frees the slot and emits
    // [ServerEventV2::PrepareUploadAborted], like a dropped connection.
    let decision = tokio::select! {
        decision = decision_rx => {
            decision.map_err(|_| AppError::Status(StatusCode::INTERNAL_SERVER_ERROR))?
        }
        _ = cancelled.cancelled() => {
            return Err(AppError::Message(
                StatusCode::FORBIDDEN,
                "Cancelled by sender".to_string(),
            ));
        }
    };

    let accepted_ids = match decision {
        PrepareUploadDecisionV2::Decline => {
            pending_guard.clear().await;
            return Err(AppError::Message(
                StatusCode::FORBIDDEN,
                "Rejected".to_string(),
            ));
        }
        PrepareUploadDecisionV2::Accept(ids) => ids,
    };

    let files: HashMap<String, SessionFileV2> = payload
        .files
        .into_iter()
        .filter(|(id, _)| accepted_ids.contains(id))
        .map(|(id, dto)| {
            let file = SessionFileV2 {
                dto,
                token: Uuid::new_v4().to_string(),
                status: FileStatusV2::Pending,
                attempts: 0,
            };
            (id, file)
        })
        .collect();

    if files.is_empty() {
        // Nothing to transfer.
        pending_guard.clear().await;
        let mut res = Response::new(empty_body());
        *res.status_mut() = StatusCode::NO_CONTENT;
        return Ok(res);
    }

    let tokens: HashMap<String, String> = files
        .iter()
        .map(|(id, file)| (id.clone(), file.token.clone()))
        .collect();

    {
        let mut slot = v2.session.lock().await;
        *slot = Some(SessionStateV2::Active(UploadSessionV2 {
            session_id: session_id.clone(),
            sender_ip: client_info.ip,
            files,
            cancel: cancelled,
            last_activity: tokio::time::Instant::now(),
        }));
    }
    pending_guard.disarm();
    // localsend-add: a peer which accepts a session and disappears must not
    // hold the only receive slot indefinitely (including checksum retries).
    watch_idle_session(Arc::downgrade(&v2), session_id.clone());

    tracing::info!("Upload session created: {session_id}");

    Ok(JsonResponse {
        status: StatusCode::OK,
        body: PrepareUploadResponseDtoV2 {
            session_id,
            files: tokens,
        },
    }
    .into_response())
}

pub(crate) async fn upload(
    req: Request<Incoming>,
    state: AppState,
    client_info: RequestClientInfo,
) -> Result<Response<BoxedBody>, AppError> {
    let v2 = require_v2(&state)?;
    let query = parse_query(req.uri().query());

    let (Some(session_id), Some(file_id), Some(token)) = (
        query.get("sessionId"),
        query.get("fileId"),
        query.get("token"),
    ) else {
        return Err(AppError::Message(
            StatusCode::BAD_REQUEST,
            "Missing parameters".to_string(),
        ));
    };

    // Validate the request and mark the file as in progress.
    let (file_dto, cancelled) = {
        let mut slot = v2.session.lock().await;
        let Some(SessionStateV2::Active(session)) = slot.as_mut() else {
            return Err(invalid_token_error());
        };
        if session.session_id != *session_id || session.sender_ip != client_info.ip {
            return Err(invalid_token_error());
        }
        let Some(file) = session.files.get_mut(file_id) else {
            return Err(invalid_token_error());
        };
        if file.token != *token || file.status != FileStatusV2::Pending {
            return Err(invalid_token_error());
        }
        file.status = FileStatusV2::InProgress;
        file.attempts = file.attempts.saturating_add(1);
        let dto = file.dto.clone();
        session.last_activity = tokio::time::Instant::now();
        (dto, session.cancel.clone())
    };

    // Marks the file as failed if this request is aborted mid-transfer.
    let mut upload_guard = UploadGuard::new(v2.clone(), session_id.clone(), file_id.clone());

    let file_size = file_dto.size;
    let expected_sha256 = match v2.verify_checksums {
        true => file_dto.sha256.clone(),
        false => None,
    };
    let timestamps = match &file_dto.metadata {
        Some(metadata) => FileTimestamps {
            modified: metadata.modified_time(),
            accessed: metadata.accessed_time(),
        },
        None => FileTimestamps::default(),
    };
    let (target_tx, target_rx) = oneshot::channel::<FileUploadTarget>();

    let event = ServerEventV2::FileUpload {
        session_id: session_id.clone(),
        file_id: file_id.clone(),
        file: file_dto,
        target_tx,
    };
    if v2.event_tx.send(event).await.is_err() {
        upload_guard.finish(SaveResult::Failed).await;
        return Err(AppError::Status(StatusCode::INTERNAL_SERVER_ERROR));
    }

    let target = tokio::select! {
        target = target_rx => target,
        _ = cancelled.cancelled() => return Err(invalid_token_error()),
    };
    let Ok(target) = target else {
        upload_guard.finish(SaveResult::Failed).await;
        return Err(AppError::Status(StatusCode::INTERNAL_SERVER_ERROR));
    };

    let save = common::save::save_req_to_target(
        req,
        target,
        file_size,
        expected_sha256.as_deref(),
        timestamps,
    );
    let result = tokio::select! {
        result = save => result,
        _ = cancelled.cancelled() => SaveResult::Failed,
    };

    upload_guard.finish(result).await;

    match result {
        SaveResult::Success => Ok(Response::new(empty_body())),
        SaveResult::Failed => Err(AppError::Status(StatusCode::INTERNAL_SERVER_ERROR)),
        SaveResult::HashMismatch => Err(AppError::Message(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Checksum mismatch".to_string(),
        )),
    }
}

pub(crate) async fn cancel(
    req: Request<Incoming>,
    state: AppState,
    client_info: RequestClientInfo,
) -> Result<Response<BoxedBody>, AppError> {
    let v2 = require_v2(&state)?;
    let query = parse_query(req.uri().query());
    let session_id = query.get("sessionId");

    // A pending prepare-upload request: the sender does not know the session
    // ID yet (it is part of the response), so a cancel from the pending
    // sender's address is accepted without one.
    let pending_cancelled = {
        let slot = v2.session.lock().await;
        match slot.as_ref() {
            Some(SessionStateV2::Pending(pending))
                if pending.sender_ip == client_info.ip
                    && session_id.is_none_or(|id| *id == pending.session_id) =>
            {
                tracing::info!(
                    "Pending upload session cancelled by sender: {}",
                    pending.session_id
                );
                // The waiting prepare-upload handler frees the slot and
                // notifies the application.
                pending.cancel.cancel();
                true
            }
            _ => false,
        }
    };

    if pending_cancelled {
        return Ok(Response::new(empty_body()));
    }

    if let Some(session_id) = session_id {
        let cancelled = {
            let mut slot = v2.session.lock().await;
            match slot.as_ref() {
                Some(SessionStateV2::Active(session))
                    if session.session_id == *session_id && session.sender_ip == client_info.ip =>
                {
                    session.cancel.cancel();
                    *slot = None;
                    true
                }
                _ => false,
            }
        };

        if cancelled {
            tracing::info!("Upload session cancelled by sender: {session_id}");
            let _ = v2
                .event_tx
                .send(ServerEventV2::SessionEnd {
                    session_id: session_id.clone(),
                    reason: SessionEndReasonV2::Cancelled,
                })
                .await;
        } else {
            // Not one of our upload sessions: the remote device may be
            // cancelling a transfer this application is sending to it.
            let _ = v2
                .event_tx
                .send(ServerEventV2::CancelReceived {
                    ip: client_info.ip,
                    session_id: session_id.clone(),
                })
                .await;
        }
    }

    Ok(Response::new(empty_body()))
}

fn require_v2(state: &AppState) -> Result<Arc<V2State>, AppError> {
    state
        .v2
        .clone()
        .ok_or(AppError::Status(StatusCode::NOT_FOUND))
}

fn invalid_token_error() -> AppError {
    AppError::Message(
        StatusCode::FORBIDDEN,
        "Invalid token or IP address".to_string(),
    )
}

/// Frees a claimed `Pending` session slot unless a session was created.
///
/// The cleanup also runs on drop so the slot is not leaked
/// when the request future is cancelled (e.g. the sender disconnected
/// while the application was still deciding).
struct PendingSessionGuard {
    v2: Arc<V2State>,
    session_id: String,
    armed: bool,
}

impl PendingSessionGuard {
    fn new(v2: Arc<V2State>, session_id: String) -> Self {
        Self {
            v2,
            session_id,
            armed: true,
        }
    }

    /// Disarms the guard after the pending slot was replaced by an active session.
    fn disarm(&mut self) {
        self.armed = false;
    }

    /// Frees the pending slot immediately.
    async fn clear(&mut self) {
        self.armed = false;
        clear_pending_session(&self.v2).await;
    }
}

impl Drop for PendingSessionGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let v2 = self.v2.clone();
        let session_id = std::mem::take(&mut self.session_id);
        tokio::spawn(async move {
            clear_pending_session(&v2).await;
            // The application may still be waiting for a decision; tell it
            // that answering is pointless now.
            let _ = v2
                .event_tx
                .send(ServerEventV2::PrepareUploadAborted { session_id })
                .await;
        });
    }
}

async fn clear_pending_session(v2: &V2State) {
    let mut slot = v2.session.lock().await;
    if matches!(*slot, Some(SessionStateV2::Pending(_))) {
        *slot = None;
    }
}

/// Sets the final status of a file after an upload attempt.
///
/// The cleanup also runs on drop (as a failure) so the file is not stuck
/// in progress when the request future is cancelled mid-transfer.
struct UploadGuard {
    v2: Arc<V2State>,
    session_id: String,
    file_id: String,
    armed: bool,
}

impl UploadGuard {
    fn new(v2: Arc<V2State>, session_id: String, file_id: String) -> Self {
        Self {
            v2,
            session_id,
            file_id,
            armed: true,
        }
    }

    async fn finish(&mut self, result: SaveResult) {
        self.armed = false;
        finalize_file(&self.v2, &self.session_id, &self.file_id, result).await;
    }
}

impl Drop for UploadGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let v2 = self.v2.clone();
        let session_id = std::mem::take(&mut self.session_id);
        let file_id = std::mem::take(&mut self.file_id);
        tokio::spawn(async move {
            finalize_file(&v2, &session_id, &file_id, SaveResult::Failed).await;
        });
    }
}

/// How often an upload of the same file may be started, i.e. how often a
/// sender may retry a file after a checksum mismatch.
/// Senders must not retry more often than this (see the upload isolate).
const MAX_UPLOAD_ATTEMPTS: u8 = 3;

/// Sets the final status of a file and ends the session once all files are done.
///
/// A checksum mismatch resets the file to [FileStatusV2::Pending] (as long as
/// [MAX_UPLOAD_ATTEMPTS] is not exhausted) so the sender can retry the upload
/// with the same token; the session stays active in that case.
async fn finalize_file(v2: &V2State, session_id: &str, file_id: &str, result: SaveResult) {
    let session_ended = {
        let mut slot = v2.session.lock().await;
        let Some(SessionStateV2::Active(session)) = slot.as_mut() else {
            return;
        };
        if session.session_id != session_id {
            return;
        }
        session.last_activity = tokio::time::Instant::now();
        if let Some(file) = session.files.get_mut(file_id) {
            if file.status == FileStatusV2::InProgress {
                file.status = match result {
                    SaveResult::Success => FileStatusV2::Finished,
                    SaveResult::HashMismatch if file.attempts < MAX_UPLOAD_ATTEMPTS => {
                        FileStatusV2::Pending
                    }
                    SaveResult::Failed | SaveResult::HashMismatch => FileStatusV2::Failed,
                };
            }
        }
        match session.is_complete() {
            true => {
                *slot = None;
                true
            }
            false => false,
        }
    };

    if session_ended {
        tracing::info!("Upload session finished: {session_id}");
        let _ = v2
            .event_tx
            .send(ServerEventV2::SessionEnd {
                session_id: session_id.to_string(),
                reason: SessionEndReasonV2::Finished,
            })
            .await;
    }
}

fn watch_idle_session(v2: std::sync::Weak<V2State>, session_id: String) {
    tokio::spawn(async move {
        loop {
            let Some(state) = v2.upgrade() else { return };
            let deadline = {
                let slot = state.session.lock().await;
                let Some(SessionStateV2::Active(session)) = slot.as_ref() else {
                    return;
                };
                if session.session_id != session_id {
                    return;
                }
                if session
                    .files
                    .values()
                    .any(|file| file.status == FileStatusV2::InProgress)
                {
                    // Active bodies have their own inactivity timeout. Do not
                    // interrupt a long transfer which is still making progress.
                    tokio::time::Instant::now() + UPLOAD_IDLE_TIMEOUT
                } else {
                    session.last_activity + UPLOAD_IDLE_TIMEOUT
                }
            };
            drop(state);
            tokio::time::sleep_until(deadline).await;
            let Some(state) = v2.upgrade() else { return };
            if expire_idle_session(&state, &session_id, tokio::time::Instant::now()).await {
                return;
            }
        }
    });
}

async fn expire_idle_session(v2: &V2State, session_id: &str, now: tokio::time::Instant) -> bool {
    let expired = {
        let mut slot = v2.session.lock().await;
        match slot.as_ref() {
            Some(SessionStateV2::Active(session))
                if session.session_id == session_id
                    && now.saturating_duration_since(session.last_activity)
                        >= UPLOAD_IDLE_TIMEOUT
                    && session
                        .files
                        .values()
                        .all(|file| file.status != FileStatusV2::InProgress) =>
            {
                session.cancel.cancel();
                *slot = None;
                true
            }
            _ => false,
        }
    };
    if expired {
        tracing::warn!("Discarding idle upload session: {session_id}");
        let _ = v2
            .event_tx
            .send(ServerEventV2::SessionEnd {
                session_id: session_id.to_string(),
                reason: SessionEndReasonV2::Cancelled,
            })
            .await;
    }
    expired
}

#[cfg(test)]
mod idle_session_tests {
    use super::*;
    use lru::LruCache;
    use std::num::NonZeroUsize;
    use tokio::sync::{mpsc, Mutex};

    #[tokio::test]
    async fn idle_watchdog_releases_only_its_own_abandoned_session() {
        let (event_tx, mut event_rx) = mpsc::channel(8);
        let cancelled = CancellationToken::new();
        let state = Arc::new(V2State {
            pin: None,
            verify_checksums: true,
            event_tx,
            session: Mutex::new(Some(SessionStateV2::Active(UploadSessionV2 {
                session_id: "abandoned".into(),
                sender_ip: PeerIp {
                    ip: "127.0.0.1".parse().unwrap(),
                    scope_id: None,
                },
                files: HashMap::new(),
                cancel: cancelled.clone(),
                last_activity: tokio::time::Instant::now() - UPLOAD_IDLE_TIMEOUT,
            }))),
            pin_attempts: Mutex::new(LruCache::new(NonZeroUsize::new(8).unwrap())),
        });
        assert!(!expire_idle_session(&state, "older-session", tokio::time::Instant::now()).await);
        assert!(!cancelled.is_cancelled());
        // Fresh activity must extend the deadline, and a live upload must not
        // be expired just because its total transfer duration is long.
        {
            let mut slot = state.session.lock().await;
            let Some(SessionStateV2::Active(session)) = slot.as_mut() else {
                unreachable!()
            };
            session.last_activity = tokio::time::Instant::now();
        }
        assert!(!expire_idle_session(&state, "abandoned", tokio::time::Instant::now()).await);
        {
            let mut slot = state.session.lock().await;
            let Some(SessionStateV2::Active(session)) = slot.as_mut() else {
                unreachable!()
            };
            session.last_activity -= UPLOAD_IDLE_TIMEOUT;
            session.files.insert(
                "live".into(),
                SessionFileV2 {
                    dto: FileDto {
                        id: "live".into(),
                        file_name: "live.bin".into(),
                        size: 10,
                        file_type: "application/octet-stream".into(),
                        sha256: None,
                        preview: None,
                        metadata: None,
                    },
                    token: "test-token".into(),
                    status: FileStatusV2::InProgress,
                    attempts: 1,
                },
            );
        }
        assert!(!expire_idle_session(&state, "abandoned", tokio::time::Instant::now()).await);
        {
            let mut slot = state.session.lock().await;
            let Some(SessionStateV2::Active(session)) = slot.as_mut() else {
                unreachable!()
            };
            session.files.get_mut("live").unwrap().status = FileStatusV2::Pending;
        }
        watch_idle_session(Arc::downgrade(&state), "abandoned".into());
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(event, ServerEventV2::SessionEnd { session_id, reason: SessionEndReasonV2::Cancelled } if session_id == "abandoned")
        );
        assert!(cancelled.is_cancelled());
        assert!(state.session.lock().await.is_none());
    }
}
