// Modified by localsend-add contributors, 2026. See NOTICE and LICENSE.
//! Persistent networking without a terminal; started explicitly or by a menu request.

use super::progress::{Phase, Transfer};
use super::receive::Answer;
use super::sending::{self, Payload};
use super::web_link::WebMode;
use super::{App, AppEvent, Network, Overlay, start_network, stop_network};
use crate::control::{self, Incoming, Listener, Request};
use crate::storage::Repository;
use crate::{sanitize, slots::Slots, ui::Ui};
use anyhow::ensure;
use localsend::discovery::{
    ChannelStatus, DeviceChannel, DiscoveryEvent, HttpChannel, StatefulDevice,
};
use localsend::http::client::LsHttpClientV2;
use localsend::model::discovery::ProtocolType;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

struct Job {
    id: String,
    target: String,
    label: String,
    payload: Payload,
    created: Instant,
}

struct Background {
    app: App,
    started: Instant,
    queue: VecDeque<Job>,
    active: Option<(String, String)>,
    completed: VecDeque<Value>,
    last_send: Option<Transfer>,
    send_detail: String,
    discovery_task: Option<tokio::task::JoinHandle<()>>,
    menu_sessions: HashMap<String, MenuSession>,
    announcement_rounds: u64,
    full_scans: u64,
    probe_rounds: u64,
    reachability: HashMap<String, Reachability>,
    probe_tx: mpsc::Sender<(String, Option<HttpChannel>)>,
    probe_limit: Arc<tokio::sync::Semaphore>,
}

const PROBE_INTERVAL: Duration = Duration::from_secs(5);
const FAST_PROBE_INTERVAL: Duration = Duration::from_secs(1);
const FAST_DISCOVERY_WINDOW: Duration = Duration::from_secs(10);
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
// MenuPoll renews this short lease. A crashed file manager cannot leave probes running.
const MENU_LEASE: Duration = Duration::from_secs(2);
const SEARCH_TIMEOUT: Duration = Duration::from_secs(4);

struct MenuSession {
    renewed: Instant,
    fast_until: Option<Instant>,
}

impl MenuSession {
    fn active(&self, now: Instant) -> bool {
        now.duration_since(self.renewed) < MENU_LEASE
    }

    fn fast_discovery(&self, now: Instant) -> bool {
        self.active(now) && self.fast_until.is_some_and(|until| now < until)
    }
}

#[derive(Default)]
struct Reachability {
    checked: Option<Instant>,
    online: bool,
    checking: bool,
    channel: Option<HttpChannel>,
}

impl Reachability {
    fn probe_due(&self, now: Instant, fast_discovery: bool) -> bool {
        let interval = if fast_discovery {
            FAST_PROBE_INTERVAL
        } else {
            PROBE_INTERVAL
        };
        !self.confirmed_online()
            && !self.checking
            && self
                .checked
                .is_none_or(|at| now.duration_since(at) >= interval)
    }

    fn confirmed_online(&self) -> bool {
        // Keep the successful result until the next menu open or explicit refresh.
        // Sending still establishes a real, certificate-checked connection.
        self.online && self.checked.is_some()
    }
}

async fn probe_peer(
    identity: Arc<crate::storage::Identity>,
    fingerprint: String,
    channels: Vec<HttpChannel>,
) -> Option<HttpChannel> {
    use futures_util::{StreamExt, stream};
    let probes = stream::iter(channels)
        .map(|channel| {
            let identity = identity.clone();
            let fingerprint = fingerprint.clone();
            async move {
                let pin = (channel.protocol == ProtocolType::Https).then(|| fingerprint.clone());
                let Ok(client) = LsHttpClientV2::try_new(
                    &identity.key_pem,
                    &identity.cert_pem,
                    pin,
                    Some(PROBE_TIMEOUT),
                ) else {
                    return None;
                };
                match client
                    .register(
                        channel.protocol,
                        &channel.host,
                        channel.port,
                        identity.register_dto(),
                    )
                    .await
                {
                    Ok(response)
                        if response
                            .cert_fingerprint
                            .as_deref()
                            .unwrap_or(&response.body.fingerprint)
                            == fingerprint =>
                    {
                        Some(channel)
                    }
                    _ => None,
                }
            }
        })
        .buffer_unordered(4);
    tokio::pin!(probes);
    while let Some(channel) = probes.next().await {
        if channel.is_some() {
            return channel;
        }
    }
    None
}

pub(crate) fn read_text(value: &str) -> anyhow::Result<String> {
    Ok(String::from_utf8(sending::read_text(value)?.to_vec())?)
}

pub(crate) async fn run(storage: Repository, mut listener: Listener) -> anyhow::Result<()> {
    std::fs::create_dir_all(&storage.destination)?;
    let Network {
        server,
        server_stop_tx,
        server_tx,
        mut server_rx,
        discovery,
        mut discovery_rx,
        discovery_stop_tx,
    } = start_network(&storage.identity).await?;
    let (events_tx, mut events_rx) = mpsc::channel(64);
    let (web_tx, mut web_rx) = mpsc::channel(16);
    let (control_tx, mut control_rx) = mpsc::channel(32);
    let (probe_tx, mut probe_rx) = mpsc::channel(128);
    let mut state = Background {
        app: App {
            ui: Ui::plain(),
            server,
            server_stop_tx: Some(server_stop_tx),
            server_tx,
            web_tx,
            web: None,
            keys: super::keys::KeyDecoder::new(),
            discovery: discovery.clone(),
            slots: Slots::new(),
            storage,
            pending: None,
            receive: None,
            last_receive: None,
            send: None,
            overlay: Overlay::None,
            preselected: Vec::new(),
            exit_error: None,
            events_tx,
        },
        started: Instant::now(),
        queue: VecDeque::new(),
        active: None,
        completed: VecDeque::new(),
        last_send: None,
        send_detail: String::new(),
        discovery_task: None,
        menu_sessions: HashMap::new(),
        announcement_rounds: 0,
        full_scans: 0,
        probe_rounds: 0,
        reachability: HashMap::new(),
        probe_tx,
        probe_limit: Arc::new(tokio::sync::Semaphore::new(16)),
    };
    state.announce();
    listener.ready();
    println!(
        "LocalSend background ready: {} (port {})",
        state.app.storage.identity.alias, state.app.storage.identity.port
    );
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let connections = std::sync::Arc::new(tokio::sync::Semaphore::new(32));
    loop {
        state.app.expire_pending(Instant::now());
        let pending_deadline = state.app.pending.as_ref().map(|request| request.deadline);
        let event = tokio::select! {
            _ = async {
                match pending_deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                state.app.expire_pending(Instant::now());
                continue;
            }
            result = listener.socket.accept() => {
                let (stream, _) = result?;
                // The socket is mode 0600; also check the peer against the socket owner.
                use std::os::unix::fs::MetadataExt;
                let owner = std::fs::metadata(control::socket_path())?.uid();
                if stream.peer_cred()?.uid() != owner { continue; }
                if let Ok(permit) = connections.clone().try_acquire_owned() {
                    let tx = control_tx.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        control::serve_connection(stream, tx).await;
                    });
                }
                continue;
            }
            Some(Incoming { request, reply }) = control_rx.recv() => {
                let stop = matches!(request, Request::Stop { .. });
                let response = state.control(request).await;
                let should_stop = stop && response["ok"] == true;
                let _ = reply.send(response);
                if should_stop { listener.stopping(); break; }
                continue;
            }
            Some(event) = server_rx.recv() => AppEvent::Server(event),
            Some(event) = discovery_rx.recv() => AppEvent::Discovery(event),
            Some(event) = events_rx.recv() => event,
            Some(event) = web_rx.recv() => AppEvent::Web(event),
            Some((fingerprint, channel)) = probe_rx.recv() => {
                let health = state.reachability.entry(fingerprint).or_default();
                health.checked = Some(Instant::now());
                health.online = channel.is_some();
                health.channel = channel;
                health.checking = false;
                continue;
            }
            _ = tick.tick() => {
                state.advance_queue();
                let now = Instant::now();
                state.menu_sessions.retain(|_, menu| menu.active(now));
                if !state.menu_sessions.is_empty() { state.check_reachability(); }
                continue;
            }
            _ = terminate.recv() => break,
            _ = interrupt.recv() => break,
        };
        state.event(event).await;
    }
    listener.stopping();
    if let Some(task) = state.discovery_task.take() {
        task.abort();
    }
    if let Some(send) = &state.app.send {
        send.cancel.token.cancel();
        // Give the send task time to notify the receiver and finish cancellation
        // before dropping the runtime and its HTTP client.
        let _ = tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(event) = events_rx.recv().await {
                if matches!(event, AppEvent::SendEnded { .. }) {
                    break;
                }
            }
        })
        .await;
    }
    if state.app.pending.is_some() {
        state.app.answer_pending(Answer::Decline);
    }
    if state.app.receive.is_some() {
        state.app.cancel_receive();
    }
    stop_network(
        &state.app.server,
        state.app.server_stop_tx.take(),
        &discovery,
        discovery_stop_tx,
    )
    .await;
    drop(listener);
    println!("LocalSend background stopped.");
    Ok(())
}

impl Background {
    fn send_progress(&self) -> Option<Transfer> {
        let send = self.app.send.as_ref()?;
        let id = self
            .active
            .as_ref()
            .map(|(id, _)| id.clone())
            .unwrap_or_default();
        if let Some(transfer) = &self.last_send
            && transfer.id == id
            && transfer.phase == Phase::Cancelled
        {
            return Some(transfer.clone());
        }
        Some(Transfer::running(
            id,
            send.alias.clone(),
            send.sent.load(Ordering::Relaxed),
            send.total_bytes,
        ))
    }

    fn panel(&self) -> Value {
        let destination = &self.app.storage.destination;
        let receive = if let Some(pending) = &self.app.pending {
            Some(
                json!({"id":pending.session_id,"alias":sanitize::single_line(&pending.alias),
                "phase":"pending","file_count":pending.files.len(),"destination":destination}),
            )
        } else if let Some(receive) = &self.app.receive {
            receive.progress().snapshot(destination)
        } else {
            self.app
                .last_receive
                .as_ref()
                .and_then(|t| t.snapshot(destination))
        };
        let send = self
            .send_progress()
            .or_else(|| {
                self.queue.front().map(|j| {
                    let mut transfer = Transfer::running(j.id.clone(), j.label.clone(), 0, 0);
                    transfer.detail = "正在连接设备".to_string();
                    transfer
                })
            })
            .and_then(|t| t.snapshot(destination))
            .or_else(|| {
                self.last_send
                    .as_ref()
                    .and_then(|t| t.snapshot(destination))
            });
        json!({"ok":true,"running":true,"pid":std::process::id(),"receive":receive,"send":send})
    }

    fn check_reachability(&mut self) {
        let now = Instant::now();
        let fast_discovery = self.fast_discovery_active(now);
        let mut candidates: BTreeMap<String, Vec<HttpChannel>> = BTreeMap::new();
        for (fingerprint, paired) in self.app.storage.paired.iter() {
            candidates.insert(
                fingerprint.clone(),
                paired.channels.iter().map(|c| c.http_channel()).collect(),
            );
        }
        for stored in self.app.discovery.devices() {
            let channels = candidates
                .entry(stored.device.fingerprint.clone())
                .or_default();
            for channel in stored
                .get_ranked_channels()
                .into_iter()
                .filter_map(|c| c.http())
            {
                if !channels.contains(channel) {
                    channels.push(channel.clone());
                }
            }
        }
        for (fingerprint, mut channels) in candidates.into_iter().take(128) {
            // An identity already reachable over TLS must not be considered
            // online merely because an HTTP peer claims the same fingerprint.
            if channels
                .iter()
                .any(|channel| channel.protocol == ProtocolType::Https)
            {
                channels.retain(|channel| channel.protocol == ProtocolType::Https);
            }
            let health = self.reachability.entry(fingerprint.clone()).or_default();
            if !health.probe_due(now, fast_discovery) {
                continue;
            }
            health.checking = true;
            self.probe_rounds += 1;
            let identity = self.app.storage.identity.clone();
            let tx = self.probe_tx.clone();
            let limit = self.probe_limit.clone();
            tokio::spawn(async move {
                let channel = tokio::time::timeout(PROBE_TIMEOUT, async {
                    let _permit = limit.acquire_owned().await.ok()?;
                    probe_peer(identity, fingerprint.clone(), channels).await
                })
                .await
                .unwrap_or(None);
                let _ = tx.send((fingerprint, channel)).await;
            });
        }
    }

    fn fast_discovery_active(&self, now: Instant) -> bool {
        self.menu_sessions
            .values()
            .any(|menu| menu.fast_discovery(now))
    }

    fn renew_menu(&mut self, session: String, opened: bool) {
        let now = Instant::now();
        let menu = self.menu_sessions.entry(session).or_insert(MenuSession {
            renewed: now,
            fast_until: None,
        });
        if !menu.active(now) {
            menu.fast_until = None;
        }
        // Polls renew only visibility. They must never extend or create a boost.
        if opened && menu.fast_until.is_none() {
            menu.fast_until = Some(now + FAST_DISCOVERY_WINDOW);
        }
        menu.renewed = now;
    }

    fn announce(&mut self) {
        if self
            .discovery_task
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return;
        }
        self.announcement_rounds += 1;
        let discovery = self.app.discovery.clone();
        self.discovery_task = Some(tokio::spawn(async move {
            let _ = tokio::time::timeout(SEARCH_TIMEOUT, discovery.announce()).await;
        }));
    }

    fn scan(&mut self, timeout: Duration) {
        let previous = self.discovery_task.take();
        if let Some(task) = &previous {
            task.abort();
        }
        self.full_scans += 1;
        self.announcement_rounds += 1;
        let discovery = self.app.discovery.clone();
        let channels = self.app.storage.paired.known_http_channels();
        // Our receiver uses 53318 to coexist with the desktop app. Unknown
        // peers normally listen on the protocol's default port, not ours.
        let port = localsend::multicast::DEFAULT_PORT;
        let interfaces = localsend::util::interface::local_interface_addresses(
            &localsend::util::interface::InterfaceFilter::default(),
        )
        .unwrap_or_default()
        .into_iter()
        .take(3)
        .collect();
        self.discovery_task = Some(tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            // Dropping the discovery future also cancels its subnet requests and
            // releases the per-interface scan guards. No detached scan survives.
            let _ = tokio::time::timeout(
                timeout.min(SEARCH_TIMEOUT),
                discovery.discover_staged(
                    channels,
                    interfaces,
                    port,
                    ProtocolType::Https,
                    Duration::from_secs(1),
                ),
            )
            .await;
        }));
    }

    fn reconfirm(&mut self) {
        for health in self.reachability.values_mut() {
            health.checked = None;
            health.online = false;
        }
        self.check_reachability();
    }

    fn snapshot(&self) -> Value {
        json!({"ok":true,"running":true,
            "searching":self.discovery_task.as_ref().is_some_and(|task| !task.is_finished()),
            "devices":self.devices()})
    }

    async fn control(&mut self, request: Request) -> Value {
        match self.try_control(request).await {
            Ok(value) => value,
            Err(error) => json!({"ok":false,"error":error.to_string()}),
        }
    }

    async fn try_control(&mut self, request: Request) -> anyhow::Result<Value> {
        self.app.expire_pending(Instant::now());
        match request {
            Request::Panel => Ok(self.panel()),
            Request::CancelTransfer {
                session_id,
                direction,
            } => {
                match direction.as_str() {
                    "receive" => {
                        ensure!(
                            self.app
                                .receive
                                .as_ref()
                                .is_some_and(|r| r.session_id == session_id),
                            "This incoming transfer has already ended"
                        );
                        self.app.cancel_receive();
                    }
                    "send" => {
                        if self
                            .active
                            .as_ref()
                            .is_some_and(|(id, _)| id == &session_id)
                        {
                            let mut transfer = self
                                .send_progress()
                                .ok_or_else(|| anyhow::anyhow!("This send has already ended"))?;
                            if transfer.phase != Phase::Cancelled {
                                transfer.finish(Phase::Cancelled);
                                self.last_send = Some(transfer);
                            }
                            self.app.send.as_ref().unwrap().cancel.token.cancel();
                        } else {
                            let index = self
                                .queue
                                .iter()
                                .position(|j| j.id == session_id)
                                .ok_or_else(|| anyhow::anyhow!("This send has already ended"))?;
                            let job = self.queue.remove(index).unwrap();
                            let mut transfer =
                                Transfer::running(job.id.clone(), job.label.clone(), 0, 0);
                            transfer.finish(Phase::Cancelled);
                            self.last_send = Some(transfer);
                            self.finish(job.id, job.label, false, "已取消排队发送");
                        }
                    }
                    _ => anyhow::bail!("Invalid transfer direction"),
                }
                Ok(self.panel())
            }
            Request::AnswerTransfer { session_id, accept } => {
                ensure!(
                    self.app
                        .pending
                        .as_ref()
                        .is_some_and(|p| p.session_id == session_id),
                    "This incoming request has already ended"
                );
                self.app.answer_pending(if accept {
                    Answer::Accept
                } else {
                    Answer::Decline
                });
                Ok(self.panel())
            }
            Request::Status => Ok(json!({
                "ok":true,"running":true,"pid":std::process::id(),
                "alias":self.app.storage.identity.alias,"port":self.app.storage.identity.port,
                "uptime_seconds":self.started.elapsed().as_secs(),
                "paired_devices":self.app.storage.paired.iter().count(),
                "listening_addresses":self.app.server.local_addresses(),
                "scheme":if self.app.web.is_some() {"http"} else {"https"},
                "web":self.web_snapshot(),
                "destination":self.app.storage.destination,
                "sending":self.app.send.as_ref().map(|s| json!({"alias":s.alias,"bytes":s.sent.load(Ordering::Relaxed),"total_bytes":s.total_bytes})),
                "receiving":self.app.receive.as_ref().map(|r| json!({"alias":r.alias,"bytes":r.done_bytes(),"total_bytes":r.total_bytes})),
                "pending_sender":self.app.pending.as_ref().map(|r| &r.alias),
                "queued":self.queue.len(),"recent_jobs":self.completed,
                "discovery":{"mode":"passive_when_menu_closed", "menu_sessions":self.menu_sessions.len(),
                    "announcement_rounds":self.announcement_rounds,"full_scans":self.full_scans,
                    "probe_rounds":self.probe_rounds,"probe_interval_seconds":PROBE_INTERVAL.as_secs(),
                    "fast_probe_interval_seconds":FAST_PROBE_INTERVAL.as_secs(),
                    "fast_window_seconds":FAST_DISCOVERY_WINDOW.as_secs(),
                    "fast_discovery_active":self.fast_discovery_active(Instant::now()),
                    "online_recheck":false,"online_result_expires":false,
                    "searching":self.discovery_task.as_ref().is_some_and(|task| !task.is_finished())}
            })),
            // Snapshot reads never initiate any network work.
            Request::Devices => Ok(self.snapshot()),
            Request::Refresh { timeout_ms } => {
                ensure!(
                    timeout_ms > 0 && timeout_ms <= 4000,
                    "Search budget must be 1..4000 ms"
                );
                self.reconfirm();
                self.scan(Duration::from_millis(timeout_ms));
                Ok(self.snapshot())
            }
            Request::MenuOpen { session } => {
                ensure!(
                    !session.is_empty() && session.len() <= 128,
                    "Invalid menu session"
                );
                ensure!(
                    self.menu_sessions.len() < 32 || self.menu_sessions.contains_key(&session),
                    "Too many menus"
                );
                self.renew_menu(session, true);
                self.reconfirm();
                self.announce();
                Ok(self.snapshot())
            }
            Request::MenuPoll { session } => {
                ensure!(
                    !session.is_empty() && session.len() <= 128,
                    "Invalid menu session"
                );
                ensure!(
                    self.menu_sessions.len() < 32 || self.menu_sessions.contains_key(&session),
                    "Too many menus"
                );
                self.renew_menu(session, false);
                Ok(self.snapshot())
            }
            Request::MenuClose { session } => {
                self.menu_sessions.remove(&session);
                Ok(self.snapshot())
            }
            Request::Send {
                target,
                paths,
                text,
            } => {
                control::validate_paths(&paths, text.is_some())?;
                ensure!(!target.trim().is_empty(), "Destination cannot be empty");
                ensure!(
                    self.queue.len() < 32,
                    "Send queue is full; wait for earlier transfers"
                );
                let payload = match text {
                    Some(text) => {
                        ensure!(!text.is_empty(), "Nothing to send");
                        Payload::Text(text.into())
                    }
                    None => Payload::Files(paths),
                };
                // Prefer the already trusted identity when a paired alias is used.
                let paired: Vec<_> = self
                    .app
                    .storage
                    .paired
                    .iter()
                    .filter(|(_, d)| d.alias == target)
                    .collect();
                ensure!(
                    paired.len() <= 1,
                    "Destination name is ambiguous; use its fingerprint"
                );
                let target = paired
                    .first()
                    .map(|(fp, _)| (*fp).clone())
                    .unwrap_or(target);
                let label = self
                    .app
                    .storage
                    .paired
                    .iter()
                    .find(|(fingerprint, _)| fingerprint.as_str() == target)
                    .map(|(_, device)| device.alias.clone())
                    .or_else(|| {
                        self.app
                            .discovery
                            .device_by_fingerprint(&target)
                            .map(|d| d.device.alias)
                    })
                    .unwrap_or_else(|| target.clone());
                if let Ok(selector) = super::target::TargetSelector::parse(&target)
                    && let Some(channel) = selector.direct_channel()
                {
                    let discovery = self.app.discovery.clone();
                    tokio::spawn(async move {
                        let _ = discovery
                            .discover(&channel.host, channel.port, channel.protocol)
                            .await;
                    });
                }
                let id = uuid::Uuid::new_v4().to_string();
                self.queue.push_back(Job {
                    id: id.clone(),
                    target,
                    label: sanitize::single_line(&label),
                    payload,
                    created: Instant::now(),
                });
                self.check_reachability();
                self.scan(SEARCH_TIMEOUT);
                Ok(json!({"ok":true,"job_id":id,"message":"已加入发送队列"}))
            }
            Request::Stop { force } => {
                ensure!(
                    force
                        || (self.app.send.is_none()
                            && self.app.receive.is_none()
                            && self.queue.is_empty()),
                    "A transfer is active or queued. Wait for completion, or use stop --force to cancel it."
                );
                Ok(json!({"ok":true,"running":false}))
            }
            Request::Pair { target } => {
                let device = self
                    .target(&target)
                    .map_err(anyhow::Error::msg)?
                    .ok_or_else(|| {
                        anyhow::anyhow!("设备尚未确认在线，请刷新设备列表后再加入白名单")
                    })?;
                let fingerprint = device.device.fingerprint.clone();
                let alias = sanitize::single_line(&device.device.alias);
                self.app.storage.paired.insert(
                    fingerprint.clone(),
                    alias.clone(),
                    crate::storage::PairedChannel::channels_of(&device),
                )?;
                Ok(
                    json!({"ok":true,"fingerprint":fingerprint,"message":format!("已将 {alias} 加入白名单")}),
                )
            }
            Request::Unpair { target } => {
                let ids: Vec<_> = self
                    .app
                    .storage
                    .paired
                    .iter()
                    .filter(|(fp, device)| fp.as_str() == target || device.alias == target)
                    .map(|(fp, _)| fp.clone())
                    .collect();
                ensure!(ids.len() == 1, "请选择唯一的白名单设备，可使用设备指纹");
                let device = self.app.storage.paired.remove(&ids[0])?.unwrap();
                Ok(
                    json!({"ok":true,"message":format!("已将 {} 移出白名单",sanitize::single_line(&device.alias))}),
                )
            }
            Request::Cancel => {
                if self.app.pending.is_some() {
                    self.app.answer_pending(Answer::Decline);
                }
                if let Some(send) = &self.app.send {
                    send.cancel.token.cancel();
                }
                if self.app.receive.is_some() {
                    self.app.cancel_receive();
                }
                while let Some(job) = self.queue.pop_front() {
                    let mut transfer = Transfer::running(job.id.clone(), job.label.clone(), 0, 0);
                    transfer.finish(Phase::Cancelled);
                    self.last_send = Some(transfer);
                    self.finish(job.id, job.label, false, "已取消排队发送");
                }
                Ok(json!({"ok":true,"message":"已取消当前传输及排队发送；后台继续运行"}))
            }
            Request::WebStatus => Ok(json!({"ok":true,"web":self.web_snapshot()})),
            request @ (Request::WebShare { .. } | Request::WebReceive | Request::WebStop) => {
                ensure!(
                    self.app.pending.is_none()
                        && self.app.receive.is_none()
                        && self.app.send.is_none()
                        && self.queue.is_empty(),
                    "收发或请求正在进行，请结束后再切换链接模式"
                );
                match request {
                    Request::WebShare { paths } => {
                        control::validate_paths(&paths, false)?;
                        ensure!(
                            self.app.web.is_none(),
                            "请先执行 localsend web stop 关闭现有链接模式"
                        );
                        self.app.enable_web_share(paths).await;
                        ensure!(
                            matches!(self.app.web, Some(WebMode::Share { .. })),
                            "链接分享启动失败，详情见后台日志"
                        );
                    }
                    Request::WebReceive => {
                        ensure!(
                            !matches!(self.app.web, Some(WebMode::Share { .. })),
                            "请先停止链接分享"
                        );
                        if self.app.web.is_none() {
                            self.app.toggle_web_receive().await;
                        }
                        ensure!(
                            matches!(self.app.web, Some(WebMode::Receive)),
                            "浏览器接收启动失败，详情见后台日志"
                        );
                    }
                    Request::WebStop => {
                        if self.app.web.is_some() {
                            self.app
                                .disable_web(crate::ui::Category::Receive, "Browser links")
                                .await;
                        }
                    }
                    _ => unreachable!(),
                }
                Ok(json!({"ok":true,"web":self.web_snapshot()}))
            }
            Request::Accept | Request::AcceptPair | Request::Decline => {
                ensure!(self.app.pending.is_some(), "No incoming request is waiting");
                self.app
                    .answer_pending(if matches!(request, Request::AcceptPair) {
                        Answer::AcceptAndPair
                    } else if matches!(request, Request::Accept) {
                        Answer::Accept
                    } else {
                        Answer::Decline
                    });
                Ok(json!({"ok":true}))
            }
        }
    }

    fn web_snapshot(&self) -> Value {
        json!({"mode":match self.app.web {Some(WebMode::Share { .. })=>"share",Some(WebMode::Receive)=>"receive",None=>"off"},
            "urls":if self.app.web.is_some() {self.app.server.local_addresses().iter().map(|address|format!("http://{address}")).collect::<Vec<_>>()}else{Vec::new()},
            "qr":if self.app.web.is_some() {self.app.web_qr()}else{None}})
    }

    fn devices(&self) -> Vec<Value> {
        let mut devices = BTreeMap::new();
        for (fingerprint, paired) in self.app.storage.paired.iter() {
            let health = self.reachability.get(fingerprint);
            devices.insert(fingerprint.clone(), json!({"alias":sanitize::single_line(&paired.alias),"fingerprint":fingerprint,"paired":true,
                "online":health.is_some_and(Reachability::confirmed_online),"checking":health.is_some_and(|h|h.checking),
                "checked":health.is_some_and(|h|h.checked.is_some())}));
        }
        for stored in self.app.discovery.devices() {
            let device = &stored.device;
            let health = self.reachability.get(&device.fingerprint);
            let online = health.is_some_and(Reachability::confirmed_online);
            devices.insert(
                device.fingerprint.clone(),
                json!({
                    "alias":sanitize::single_line(&device.alias),"fingerprint":device.fingerprint,
                    "paired":self.app.storage.paired.contains(&device.fingerprint),"online":online,
                    "checking":health.is_some_and(|h|h.checking),"checked":health.is_some_and(|h|h.checked.is_some())
                }),
            );
        }
        let mut devices: Vec<_> = devices.into_values().collect();
        devices.sort_by_key(|v| {
            (
                !v["online"].as_bool().unwrap_or(false),
                v["alias"].as_str().unwrap_or("").to_string(),
            )
        });
        devices
    }

    fn target(&self, target: &str) -> Result<Option<StatefulDevice>, String> {
        let confirmed = |fingerprint: &str| {
            self.reachability
                .get(fingerprint)
                .is_some_and(Reachability::confirmed_online)
        };
        if let Some(device) = self.app.discovery.device_by_fingerprint(target)
            && confirmed(target)
        {
            return Ok(Some(self.use_verified_channel(device)));
        }
        let ip = target.parse::<std::net::IpAddr>().ok();
        let matches: Vec<_> = self
            .app
            .discovery
            .devices()
            .into_iter()
            .filter(|d| confirmed(&d.device.fingerprint))
            .filter(|d| {
                d.device.alias == target
                    || ip.is_some_and(|ip| {
                        d.get_ranked_channels().iter().any(|c| {
                            c.http().is_some_and(|h| {
                                h.host
                                    .split('%')
                                    .next()
                                    .and_then(|s| s.parse::<std::net::IpAddr>().ok())
                                    == Some(ip)
                            })
                        })
                    })
            })
            .collect();
        match matches.len() {
            0 => Ok(None),
            1 => Ok(matches
                .into_iter()
                .next()
                .map(|device| self.use_verified_channel(device))),
            _ => Err("存在多个同名设备，请使用设备身份选择目标".to_string()),
        }
    }

    fn use_verified_channel(&self, mut device: StatefulDevice) -> StatefulDevice {
        if let Some(channel) = self
            .reachability
            .get(&device.device.fingerprint)
            .and_then(|health| health.channel.clone())
        {
            let channel = DeviceChannel::Http(channel);
            device.device.channel = channel.clone();
            device.channels = HashMap::from([(channel, ChannelStatus::Available)]);
        }
        device
    }

    fn advance_queue(&mut self) {
        if self.app.send.is_some() {
            return;
        }
        let Some(job) = self.queue.pop_front() else {
            return;
        };
        self.check_reachability();
        match self.target(&job.target) {
            Ok(Some(device)) => {
                let alias = sanitize::single_line(&device.device.alias);
                let result = match job.payload {
                    Payload::Files(paths) => sending::spawn_send(
                        &mut self.app.ui,
                        self.app.storage.identity.clone(),
                        device,
                        paths,
                        self.app.events_tx.clone(),
                    ),
                    Payload::Text(text) => sending::spawn_send_text(
                        &mut self.app.ui,
                        self.app.storage.identity.clone(),
                        device,
                        text,
                        self.app.events_tx.clone(),
                    ),
                };
                match result {
                    Ok(send) => {
                        self.last_send = None;
                        self.send_detail.clear();
                        self.app.send = Some(send);
                        self.active = Some((job.id, alias));
                    }
                    Err(error) => self.finish(job.id, alias, false, &error),
                }
            }
            Ok(None) if job.created.elapsed() < Duration::from_secs(25) => {
                self.queue.push_front(job);
            }
            Ok(None) => self.finish(
                job.id,
                job.label,
                false,
                "未找到设备，请打开手机上的 LocalSend 并连接同一局域网",
            ),
            Err(error) => self.finish(job.id, job.label, false, &error),
        }
    }

    fn finish(&mut self, id: String, alias: String, success: bool, message: &str) {
        if !success && self.last_send.as_ref().is_none_or(|t| t.id != id) {
            let mut transfer = Transfer::running(id.clone(), alias.clone(), 0, 0);
            transfer.detail = message.to_string();
            transfer.finish(Phase::Failed);
            self.last_send = Some(transfer);
        }
        self.completed
            .push_front(json!({"id":id,"alias":alias,"success":success,"message":message}));
        self.completed.truncate(20);
    }

    async fn event(&mut self, event: AppEvent) {
        if matches!(
            &event,
            AppEvent::Discovery(DiscoveryEvent::Discovered { .. } | DiscoveryEvent::Updated { .. })
        ) && (!self.menu_sessions.is_empty() || !self.queue.is_empty())
        {
            self.check_reachability();
        }
        if let AppEvent::Log {
            category: crate::ui::Category::Send,
            text,
        } = &event
        {
            self.send_detail = text.clone();
        }
        if let AppEvent::SendEnded { success, cancelled } = &event
            && let Some((id, alias)) = self.active.take()
        {
            let cancelled_early = self
                .last_send
                .as_ref()
                .filter(|t| t.id == id && t.phase == Phase::Cancelled)
                .cloned();
            let mut transfer = cancelled_early.clone().unwrap_or_else(|| {
                self.send_progress()
                    .unwrap_or_else(|| Transfer::running(id.clone(), alias.clone(), 0, 0))
            });
            transfer.id = id.clone();
            transfer.detail = self.send_detail.clone();
            if cancelled_early.is_none() {
                transfer.finish(if *success {
                    Phase::Success
                } else if *cancelled {
                    Phase::Cancelled
                } else {
                    Phase::Failed
                });
            }
            self.last_send = Some(transfer);
            self.finish(
                id,
                alias,
                *success,
                if *success {
                    "文件已发送"
                } else {
                    "传输未完成，请检查手机和网络；详情见 background.log"
                },
            );
        }
        self.app.dispatch(event).await;
    }
}

#[cfg(test)]
mod reachability_tests {
    use super::*;

    #[test]
    fn fast_retries_apply_only_to_offline_peers_without_overlapping() {
        let checked = Instant::now();
        let mut health = Reachability {
            checked: Some(checked),
            ..Default::default()
        };
        assert!(!health.probe_due(checked + Duration::from_millis(999), true));
        assert!(health.probe_due(checked + Duration::from_secs(1), true));
        assert!(!health.probe_due(checked + Duration::from_secs(1), false));
        health.checking = true;
        assert!(!health.probe_due(checked + Duration::from_secs(5), true));
        health.checking = false;
        health.online = true;
        assert!(!health.probe_due(checked + Duration::from_secs(1), true));
        assert!(!health.probe_due(checked + Duration::from_secs(5), true));
        assert!(!health.probe_due(checked + Duration::from_secs(3600), false));
    }

    #[test]
    fn fast_window_requires_a_live_menu_lease() {
        let now = Instant::now();
        let mut menu = MenuSession {
            renewed: now,
            fast_until: Some(now + FAST_DISCOVERY_WINDOW),
        };
        assert!(menu.fast_discovery(now));
        assert!(!menu.fast_discovery(now + MENU_LEASE));
        menu.renewed = now + FAST_DISCOVERY_WINDOW;
        assert!(!menu.fast_discovery(now + FAST_DISCOVERY_WINDOW));
    }

    #[test]
    fn online_result_needs_confirmation_but_does_not_expire() {
        let mut health = Reachability {
            online: true,
            ..Default::default()
        };
        assert!(!health.confirmed_online());
        health.checked = Some(Instant::now());
        assert!(health.confirmed_online());
        health.checked = Some(Instant::now() - Duration::from_secs(7));
        assert!(health.confirmed_online());
        health.checked = Some(Instant::now() - Duration::from_secs(3600));
        assert!(health.confirmed_online());
    }

    #[test]
    fn a_failed_probe_revokes_online_immediately() {
        let health = Reachability {
            checked: Some(Instant::now()),
            online: false,
            checking: false,
            channel: None,
        };
        assert!(!health.confirmed_online());
    }
}

#[cfg(test)]
#[path = "background_tests.rs"]
mod migration_tests;
