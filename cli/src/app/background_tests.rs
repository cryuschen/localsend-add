//! Local-only migration checks: no multicast interfaces or announcements.
use super::*;
use crate::storage::{Identity, PairedDevices};
use localsend::discovery::{DeviceIdentity, DiscoveryConfig};
use localsend::http::dto_v2::PrepareUploadRequestDtoV2;
use localsend::http::server::v2::ServerEventV2;
use localsend::http::server::web::{WebConfig, WebDownloadEvent};
use localsend::http::server::{ServerConfigV2, start_with_port};
use localsend::model::transfer::FileDto;
use localsend::util::interface::InterfaceFilter;
use std::path::PathBuf;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

struct Fixture {
    state: Background,
    server_rx: mpsc::Receiver<ServerEventV2>,
    events_rx: mpsc::Receiver<AppEvent>,
    web_rx: mpsc::Receiver<WebDownloadEvent>,
    probe_rx: mpsc::Receiver<(String, Option<HttpChannel>)>,
    discovery_stop: Option<oneshot::Sender<()>>,
    dir: PathBuf,
}
impl Fixture {
    async fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("localsend-unified-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let identity =
            Arc::new(Identity::load_or_generate(&dir, "unified-test".into(), 0).unwrap());
        let (server_tx, server_rx) = mpsc::channel(32);
        let (stop_tx, stop_rx) = oneshot::channel();
        let server = Arc::new(
            start_with_port(
                0,
                Some(identity.tls_config()),
                identity.client_info(),
                None,
                Some(ServerConfigV2 {
                    pin: None,
                    verify_checksums: true,
                    event_tx: server_tx.clone(),
                }),
                WebConfig::default(),
                stop_rx,
            )
            .await
            .unwrap(),
        );
        let (discovery_stop, discovery_rx) = oneshot::channel();
        let discovery = Arc::new(
            localsend::discovery::start(
                DiscoveryConfig {
                    group: localsend::multicast::DEFAULT_MULTICAST_GROUP,
                    group_v6: None,
                    port: 0,
                    interface_filter: InterfaceFilter {
                        whitelist: Some(vec![]),
                        blacklist: None,
                    },
                    device: identity.multicast_device(),
                    identity: DeviceIdentity {
                        cert_pem: identity.cert_pem.clone(),
                        private_key_pem: identity.key_pem.clone(),
                    },
                    timeout: Duration::from_millis(100),
                    event_tx: None,
                },
                discovery_rx,
            )
            .await,
        );
        assert!(discovery.multicast_error().is_some());
        let (events_tx, events_rx) = mpsc::channel(64);
        let (web_tx, web_rx) = mpsc::channel(32);
        let (probe_tx, probe_rx) = mpsc::channel(8);
        let state = Background {
            app: App {
                ui: Ui::plain(),
                server,
                server_stop_tx: Some(stop_tx),
                server_tx,
                web_tx,
                web: None,
                keys: super::super::keys::KeyDecoder::new(),
                discovery,
                slots: Slots::new(),
                storage: Repository {
                    dir: dir.clone(),
                    destination: dir.clone(),
                    identity,
                    paired: PairedDevices::load(&dir).unwrap(),
                },
                pending: None,
                receive: None,
                last_receive: None,
                send: None,
                overlay: Overlay::None,
                preselected: vec![],
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
            probe_limit: Arc::new(tokio::sync::Semaphore::new(1)),
        };
        Self {
            state,
            server_rx,
            events_rx,
            web_rx,
            probe_rx,
            discovery_stop: Some(discovery_stop),
            dir,
        }
    }
    async fn receive_probe(&mut self) -> Option<HttpChannel> {
        let (fingerprint, channel) =
            tokio::time::timeout(Duration::from_secs(3), self.probe_rx.recv())
                .await
                .unwrap()
                .unwrap();
        let health = self.state.reachability.entry(fingerprint).or_default();
        health.checked = Some(Instant::now());
        health.online = channel.is_some();
        health.channel = channel.clone();
        health.checking = false;
        channel
    }
    async fn pump(&mut self, done: impl Fn(&Background) -> bool) {
        tokio::time::timeout(Duration::from_secs(8), async {
            while !done(&self.state) {
                let event = tokio::select! {
                    Some(event)=self.server_rx.recv()=>AppEvent::Server(event),
                    Some(event)=self.events_rx.recv()=>event,
                    Some(event)=self.web_rx.recv()=>AppEvent::Web(event),
                };
                self.state.event(event).await;
            }
        })
        .await
        .expect("local event processing timed out");
    }
    async fn finish(mut self) {
        stop_network(
            &self.state.app.server,
            self.state.app.server_stop_tx.take(),
            &self.state.app.discovery,
            self.discovery_stop.take().unwrap(),
        )
        .await;
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
async fn menu_boost_is_bounded_and_snapshot_reads_do_not_start_it() {
    let mut f = Fixture::new().await;
    assert!(!f.state.fast_discovery_active(Instant::now()));
    f.state.control(Request::Devices).await;
    f.state
        .control(Request::MenuPoll {
            session: "menu".into(),
        })
        .await;
    assert!(!f.state.fast_discovery_active(Instant::now()));
    f.state
        .control(Request::MenuOpen {
            session: "menu".into(),
        })
        .await;
    let deadline = f.state.menu_sessions["menu"].fast_until.unwrap();
    assert!(f.state.fast_discovery_active(Instant::now()));
    f.state
        .control(Request::MenuPoll {
            session: "menu".into(),
        })
        .await;
    assert_eq!(f.state.menu_sessions["menu"].fast_until, Some(deadline));
    f.state
        .control(Request::MenuOpen {
            session: "menu".into(),
        })
        .await;
    assert_eq!(f.state.menu_sessions["menu"].fast_until, Some(deadline));
    f.state.menu_sessions.get_mut("menu").unwrap().fast_until = Some(Instant::now());
    f.state
        .control(Request::MenuPoll {
            session: "menu".into(),
        })
        .await;
    assert!(!f.state.fast_discovery_active(Instant::now()));
    f.state
        .control(Request::MenuClose {
            session: "menu".into(),
        })
        .await;
    assert!(f.state.menu_sessions.is_empty());
    f.state
        .control(Request::MenuOpen {
            session: "menu".into(),
        })
        .await;
    assert!(f.state.fast_discovery_active(Instant::now()));
    f.state.menu_sessions.get_mut("menu").unwrap().renewed = Instant::now() - MENU_LEASE;
    assert!(!f.state.fast_discovery_active(Instant::now()));
    // A late poll can restore visibility, but cannot restart an expired boost.
    f.state
        .control(Request::MenuPoll {
            session: "menu".into(),
        })
        .await;
    assert!(!f.state.fast_discovery_active(Instant::now()));
    assert_eq!(f.state.full_scans, 0);
    f.finish().await;
}

#[tokio::test]
async fn fast_retry_confirms_tls_then_keeps_result_without_more_probes() {
    use crate::storage::PairedChannel;
    let mut f = Fixture::new().await;
    let peer = Fixture::new().await;
    let fingerprint = peer.state.app.storage.identity.fingerprint.clone();
    // Reserve a loopback address that is not yet serving LocalSend.
    let unused = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = unused.local_addr().unwrap().port();
    drop(unused);
    let channel = |port| PairedChannel {
        host: "127.0.0.1".into(),
        port,
        protocol: ProtocolType::Https,
    };
    f.state
        .app
        .storage
        .paired
        .insert(fingerprint.clone(), "late-peer".into(), vec![channel(port)])
        .unwrap();
    f.state
        .control(Request::MenuOpen {
            session: "menu".into(),
        })
        .await;
    assert!(f.receive_probe().await.is_none());
    let rounds = f.state.probe_rounds;
    f.state
        .app
        .storage
        .paired
        .update(
            &fingerprint,
            "late-peer",
            vec![channel(peer.state.app.server.port())],
        )
        .unwrap();
    f.state.check_reachability();
    assert_eq!(f.state.probe_rounds, rounds);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    f.state
        .control(Request::MenuPoll {
            session: "menu".into(),
        })
        .await;
    f.state.check_reachability();
    assert_eq!(f.state.probe_rounds, rounds + 1);
    let verified_channel = f.receive_probe().await.unwrap();
    let info = peer.state.app.storage.identity.register_dto();
    f.state
        .app
        .discovery
        .add_device(localsend::discovery::DiscoveredDevice {
            alias: info.alias,
            version: info.version,
            device_model: info.device_model,
            device_type: info.device_type,
            fingerprint: info.fingerprint,
            download: info.download,
            channel: DeviceChannel::Http(verified_channel),
        })
        .await;
    assert_eq!(f.state.devices()[0]["online"], true);
    // Even if the peer goes away, this menu retains the successful result.
    peer.finish().await;
    f.state.reachability.get_mut(&fingerprint).unwrap().checked =
        Some(Instant::now() - Duration::from_secs(3600));
    for _ in 0..16 {
        f.state
            .control(Request::MenuPoll {
                session: "menu".into(),
            })
            .await;
        f.state.check_reachability();
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    assert_eq!(f.state.probe_rounds, rounds + 1);
    assert_eq!(f.state.devices()[0]["online"], true);
    // The same non-expiring result must also remain usable by the send queue.
    assert!(f.state.target(&fingerprint).unwrap().is_some());
    f.state
        .control(Request::MenuClose {
            session: "menu".into(),
        })
        .await;
    f.state
        .control(Request::MenuOpen {
            session: "menu".into(),
        })
        .await;
    assert!(f.receive_probe().await.is_none());
    assert_eq!(f.state.devices()[0]["online"], false);
    assert!(f.state.target(&fingerprint).unwrap().is_none());
    f.state.reachability.get_mut(&fingerprint).unwrap().checked =
        Some(Instant::now() - FAST_PROBE_INTERVAL);
    // A peer that has not been confirmed in the new menu still gets fast retries.
    f.state.check_reachability();
    assert_eq!(f.state.probe_rounds, rounds + 3);
    assert!(f.receive_probe().await.is_none());
    f.finish().await;
}

fn file(preview: Option<&str>) -> FileDto {
    serde_json::from_value(json!({"id":"file","fileName":"中文资料.txt","size":6,
        "fileType":"text/plain","preview":preview}))
    .unwrap()
}

#[tokio::test]
async fn unified_management_preserves_real_tls_receiving_and_browser_modes() {
    let mut f = Fixture::new().await;
    let peer_dir = f.dir.join("peer");
    std::fs::create_dir(&peer_dir).unwrap();
    let peer = Arc::new(Identity::load_or_generate(&peer_dir, "peer".into(), 53317).unwrap());
    let server_fingerprint = f.state.app.storage.identity.fingerprint.clone();
    let offer = |file: FileDto, port: u16| {
        let peer = peer.clone();
        let pin = server_fingerprint.clone();
        tokio::spawn(async move {
            let client = LsHttpClientV2::try_new(
                &peer.key_pem,
                &peer.cert_pem,
                Some(pin),
                Some(Duration::from_secs(8)),
            )
            .unwrap();
            client
                .prepare_upload(
                    ProtocolType::Https,
                    "127.0.0.1",
                    port,
                    None,
                    PrepareUploadRequestDtoV2 {
                        info: peer.register_dto(),
                        files: HashMap::from([("file".into(), file)]),
                    },
                    None,
                    CancellationToken::new(),
                )
                .await
        })
    };
    // An untrusted text first waits for the same explicit decision as a file.
    let request = offer(file(Some("中文\n消息")), f.state.app.server.port());
    f.pump(|s| s.app.pending.is_some()).await;
    assert_eq!(f.state.panel()["receive"]["phase"], "pending");
    assert!(
        !f.state.control(Request::WebReceive).await["ok"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(f.state.control(Request::AcceptPair).await["ok"], true);
    f.pump(|s| s.app.receive.is_none() && s.app.last_receive.is_some())
        .await;
    assert_eq!(request.await.unwrap().unwrap().status_code, 204);
    let result = f.state.panel();
    assert_eq!(result["receive"]["phase"], "success");
    let path = result["receive"]["paths"][0].as_str().unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "中文\n消息");
    assert!(f.state.app.storage.paired.contains(&peer.fingerprint));
    assert!(
        PairedDevices::load(&f.dir)
            .unwrap()
            .contains(&peer.fingerprint)
    );
    f.state.reachability.insert(
        peer.fingerprint.clone(),
        Reachability {
            checked: Some(Instant::now()),
            online: true,
            checking: false,
            channel: None,
        },
    );
    assert_eq!(
        f.state
            .control(Request::Unpair {
                target: "peer".into()
            })
            .await["ok"],
        true
    );
    assert!(!f.state.app.storage.paired.contains(&peer.fingerprint));
    assert_eq!(
        f.state
            .control(Request::Pair {
                target: "unknown".into()
            })
            .await["ok"],
        false
    );
    // Cancellation declines the pending request without stopping the backend.
    let request = offer(file(None), f.state.app.server.port());
    f.pump(|s| s.app.pending.is_some()).await;
    assert_eq!(f.state.control(Request::Cancel).await["ok"], true);
    assert!(request.await.unwrap().is_err());
    assert!(f.state.app.pending.is_none());
    assert_eq!(
        f.state
            .control(Request::Pair {
                target: peer.fingerprint.clone()
            })
            .await["ok"],
        true
    );
    // Trusted regular files still use the original prepare/upload TLS protocol.
    let request = offer(file(None), f.state.app.server.port());
    f.pump(|s| s.app.receive.is_some()).await;
    let prepared = request.await.unwrap().unwrap().response.unwrap();
    let port = f.state.app.server.port();
    let upload_peer = peer.clone();
    let upload = tokio::spawn(async move {
        let client = LsHttpClientV2::try_new(
            &upload_peer.key_pem,
            &upload_peer.cert_pem,
            Some(server_fingerprint),
            None,
        )
        .unwrap();
        client
            .upload(
                ProtocolType::Https,
                "127.0.0.1",
                port,
                None,
                &prepared.session_id,
                "file",
                &prepared.files["file"],
                b"abcdef".to_vec().into(),
                CancellationToken::new(),
            )
            .await
    });
    f.pump(|s| s.app.receive.is_none()).await;
    upload.await.unwrap().unwrap();
    assert_eq!(
        std::fs::read(f.dir.join("中文资料.txt")).unwrap(),
        b"abcdef"
    );
    assert_eq!(f.state.panel()["receive"]["phase"], "success");
    // Both browser modes are now managed by the same backend and original server code.
    let share = f
        .state
        .control(Request::WebShare {
            paths: vec![f.dir.join("中文资料.txt")],
        })
        .await;
    assert_eq!(share["ok"], true);
    assert_eq!(share["web"]["mode"], "share");
    assert_eq!(f.state.control(Request::Status).await["scheme"], "http");
    // The migrated event receiver still serves content requests.
    let (decision_tx, decision_rx) = oneshot::channel();
    f.state
        .event(AppEvent::Web(WebDownloadEvent::PrepareDownload {
            ip: localsend::http::server::PeerIp {
                ip: "127.0.0.1".parse().unwrap(),
                scope_id: None,
            },
            session_id: "web".into(),
            user_agent: None,
            decision_tx,
        }))
        .await;
    assert!(decision_rx.await.unwrap());
    assert_eq!(
        f.state.control(Request::WebStop).await["web"]["mode"],
        "off"
    );
    assert_eq!(f.state.control(Request::Status).await["scheme"], "https");
    assert_eq!(
        f.state.control(Request::WebReceive).await["web"]["mode"],
        "receive"
    );
    assert_eq!(f.state.control(Request::WebStop).await["ok"], true);
    assert_eq!(f.state.announcement_rounds, 0);
    f.finish().await;
}
