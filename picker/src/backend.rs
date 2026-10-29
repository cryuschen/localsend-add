//! Same-user IPC only: discovery, confirmation policy and sending stay in the CLI.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const MAX_RESPONSE: u64 = 1024 * 1024;
const MAX_REQUEST: usize = 256 * 1024;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Device {
    pub fingerprint: String,
    #[serde(default)]
    pub alias: String,
    #[serde(default)]
    pub online: bool,
    #[serde(default)]
    pub paired: bool,
}

#[derive(Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Request {
    MenuOpen {
        session: String,
    },
    MenuPoll {
        session: String,
    },
    MenuClose {
        session: String,
    },
    Refresh {
        timeout_ms: u64,
    },
    Send {
        target: String,
        paths: Vec<PathBuf>,
        text: Option<String>,
    },
}

pub enum Job {
    Open,
    Poll,
    Refresh,
    Send {
        target: String,
        paths: Vec<PathBuf>,
        text: Option<String>,
    },
    Close,
}

pub enum Reply {
    Devices(std::result::Result<Vec<Device>, String>),
    Sent(std::result::Result<(), String>),
}

pub fn socket_path() -> Result<PathBuf> {
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .context("Cannot locate the configuration directory")?;
    Ok(root.join("localsend-cli/control.sock"))
}

fn exchange(stream: UnixStream, request: &Request) -> Result<Value> {
    stream.set_read_timeout(Some(Duration::from_secs(6)))?;
    stream.set_write_timeout(Some(Duration::from_secs(6)))?;
    let mut writer = &stream;
    let encoded = serde_json::to_vec(request)?;
    ensure!(
        encoded.len() < MAX_REQUEST,
        "发送内容过长，请保存为文件后发送。"
    );
    writer.write_all(&encoded)?;
    writer.write_all(b"\n")?;
    let mut bytes = Vec::new();
    BufReader::new(stream.take(MAX_RESPONSE + 1)).read_until(b'\n', &mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_RESPONSE && bytes.last() == Some(&b'\n'),
        "Invalid device response size"
    );
    let response: Value = serde_json::from_slice(&bytes)?;
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        bail!(
            "{}",
            response
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("Invalid backend response")
        );
    }
    Ok(response)
}

fn request(socket: &Path, request: &Request) -> Result<Value> {
    exchange(
        UnixStream::connect(socket).context("Cannot connect to the LocalSend backend")?,
        request,
    )
}

pub fn visible_devices(devices: Vec<Device>) -> Vec<Device> {
    devices
        .into_iter()
        .filter(|device| !device.fingerprint.is_empty() && (device.paired || device.online))
        .take(128)
        .collect()
}

pub struct Worker {
    pub requests: Sender<Job>,
    pub replies: futures_channel::mpsc::UnboundedReceiver<Reply>,
    pub thread: JoinHandle<()>,
}

pub fn start(socket: PathBuf) -> Worker {
    let (requests, jobs) = mpsc::channel();
    let (results, replies) = futures_channel::mpsc::unbounded();
    let thread = thread::spawn(move || {
        let session = format!("panel-{}", uuid::Uuid::new_v4());
        // A single worker orders close after any in-flight open/poll/send. This
        // prevents a late open from resurrecting a closed discovery session.
        while let Ok(job) = jobs.recv() {
            let command = match job {
                Job::Open => Request::MenuOpen {
                    session: session.clone(),
                },
                Job::Poll => Request::MenuPoll {
                    session: session.clone(),
                },
                Job::Refresh => Request::Refresh { timeout_ms: 4000 },
                Job::Send {
                    target,
                    paths,
                    text,
                } => Request::Send {
                    target,
                    paths,
                    text,
                },
                Job::Close => break,
            };
            let result = request(&socket, &command);
            let reply = if matches!(command, Request::Send { .. }) {
                Reply::Sent(result.map(|_| ()).map_err(|error| format!("{error:#}")))
            } else {
                Reply::Devices(
                    result
                        .and_then(|response| {
                            let devices = response.get("devices").context("Missing device list")?;
                            Ok(visible_devices(serde_json::from_value(devices.clone())?))
                        })
                        .map_err(|error| format!("{error:#}")),
                )
            };
            if results.unbounded_send(reply).is_err() {
                break;
            }
        }
        let _ = request(&socket, &Request::MenuClose { session });
    });
    Worker {
        requests,
        replies,
        thread,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::net::UnixListener;

    #[test]
    fn rpc_preserves_paths_and_rejects_bad_frames_and_backend_errors() {
        for (index, payload) in [
            b"{\"ok\":true}\n".to_vec(),
            b"{\"ok\":true}".to_vec(),
            b"{\"ok\":false,\"error\":\"queue full\"}\n".to_vec(),
            vec![b'x'; MAX_RESPONSE as usize + 1],
        ]
        .into_iter()
        .enumerate()
        {
            let (client, server) = UnixStream::pair().unwrap();
            let peer = thread::spawn(move || {
                let mut line = String::new();
                let mut reader = BufReader::new(server);
                reader.read_line(&mut line).unwrap();
                let value: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(
                    value["paths"],
                    json!(["/tmp/中文 folder/$(touch no);\n.txt"])
                );
                let _ = reader.get_mut().write_all(&payload);
            });
            let result = exchange(
                client,
                &Request::Send {
                    target: "FP".into(),
                    paths: vec![PathBuf::from("/tmp/中文 folder/$(touch no);\n.txt")],
                    text: None,
                },
            );
            if index == 0 {
                assert_eq!(result.unwrap(), json!({"ok":true}));
            } else {
                assert!(result.is_err(), "malformed or rejected replies must fail");
            }
            peer.join().unwrap();
        }
    }

    #[test]
    fn filters_before_row_limit_and_retains_paired_offline_devices() {
        let mut devices = vec![
            Device {
                fingerprint: "STALE".into(),
                alias: "old".into(),
                online: false,
                paired: false
            };
            150
        ];
        devices.push(Device {
            fingerprint: "LIVE".into(),
            alias: "手机".into(),
            online: true,
            paired: false,
        });
        devices.push(Device {
            fingerprint: "OFF".into(),
            alias: "平板".into(),
            online: false,
            paired: true,
        });
        assert_eq!(
            visible_devices(devices)
                .iter()
                .map(|d| d.fingerprint.as_str())
                .collect::<Vec<_>>(),
            ["LIVE", "OFF"]
        );
    }

    #[test]
    fn text_request_preserves_multiline_unicode_and_rejects_oversized_json() {
        let text = "中文🙂\n\"引号\"\\路径\n$(touch no)\n";
        let (client, server) = UnixStream::pair().unwrap();
        let peer = thread::spawn(move || {
            let mut reader = BufReader::new(server);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let value: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(value["text"], text);
            assert_eq!(value["paths"], json!([]));
            reader.get_mut().write_all(b"{\"ok\":true}\n").unwrap();
        });
        exchange(
            client,
            &Request::Send {
                target: "LIVE".into(),
                paths: Vec::new(),
                text: Some(text.into()),
            },
        )
        .unwrap();
        peer.join().unwrap();

        // 64 KB of control characters can exceed the 256 KB IPC frame after escaping.
        let (client, _server) = UnixStream::pair().unwrap();
        let error = exchange(
            client,
            &Request::Send {
                target: "LIVE".into(),
                paths: Vec::new(),
                text: Some("\u{0001}".repeat(64_000)),
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("发送内容过长"));
    }

    #[test]
    fn worker_orders_session_cleanup_after_pending_query_and_send() {
        let directory =
            std::env::temp_dir().join(format!("localsend-picker-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let socket = directory.join("control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let peer = thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..6 {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let value: Value = serde_json::from_str(&line).unwrap();
                seen.push(value);
                reader
                    .get_mut()
                    .write_all(b"{\"ok\":true,\"devices\":[]}\n")
                    .unwrap();
            }
            assert_eq!(
                seen.iter()
                    .map(|v| v["command"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                [
                    "menu_open",
                    "menu_poll",
                    "refresh",
                    "send",
                    "send",
                    "menu_close"
                ]
            );
            assert_eq!(seen[0]["session"], seen[1]["session"]);
            assert_eq!(seen[0]["session"], seen[5]["session"]);
            assert_eq!(seen[3]["target"], "LIVE");
            assert_eq!(seen[3]["paths"], json!(["/tmp/中文 folder"]));
            assert!(seen[3]["text"].is_null());
            assert_eq!(seen[4]["paths"], json!([]));
            assert_eq!(seen[4]["text"], "消息\n第二行🙂\n");
        });
        let worker = start(socket);
        worker.requests.send(Job::Open).unwrap();
        worker.requests.send(Job::Poll).unwrap();
        worker.requests.send(Job::Refresh).unwrap();
        worker
            .requests
            .send(Job::Send {
                target: "LIVE".into(),
                paths: vec!["/tmp/中文 folder".into()],
                text: None,
            })
            .unwrap();
        worker
            .requests
            .send(Job::Send {
                target: "LIVE".into(),
                paths: Vec::new(),
                text: Some("消息\n第二行🙂\n".into()),
            })
            .unwrap();
        worker.requests.send(Job::Close).unwrap();
        worker.thread.join().unwrap();
        peer.join().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
