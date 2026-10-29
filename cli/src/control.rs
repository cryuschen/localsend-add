//! Same-user, local control of one on-demand background process.

use crate::{Args, Command, WebCommand, app, storage};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command as Process, Stdio};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

pub(crate) const MAX_REQUEST: usize = 256 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Request {
    Status,
    Panel,
    AnswerTransfer {
        session_id: String,
        accept: bool,
    },
    Devices,
    Refresh {
        timeout_ms: u64,
    },
    MenuOpen {
        session: String,
    },
    MenuPoll {
        session: String,
    },
    MenuClose {
        session: String,
    },
    Send {
        target: String,
        paths: Vec<PathBuf>,
        text: Option<String>,
    },
    Stop {
        force: bool,
    },
    Accept,
    AcceptPair,
    Decline,
    Pair {
        target: String,
    },
    Unpair {
        target: String,
    },
    Cancel,
    WebShare {
        paths: Vec<PathBuf>,
    },
    WebReceive,
    WebStop,
    WebStatus,
}

pub(crate) struct Incoming {
    pub request: Request,
    pub reply: oneshot::Sender<Value>,
}

pub(crate) fn socket_path() -> PathBuf {
    storage::dir().join("control.sock")
}

pub(crate) struct Listener {
    pub socket: UnixListener,
    path: PathBuf,
    _lock: File,
}

impl Listener {
    pub fn bind() -> anyhow::Result<Self> {
        let path = socket_path();
        let directory = path.parent().unwrap();
        std::fs::create_dir_all(directory)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(directory.join("background.lock"))?;
        lock.try_lock()
            .context("A LocalSend background process is already starting or running")?;
        if path.symlink_metadata().is_ok() {
            ensure!(
                path.symlink_metadata()?.file_type().is_socket(),
                "Refusing to replace a non-socket control path"
            );
            std::fs::remove_file(&path)?;
        }
        let socket = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            socket,
            path,
            _lock: lock,
        })
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(crate) async fn serve_connection(stream: UnixStream, tx: mpsc::Sender<Incoming>) {
    let result = tokio::time::timeout(Duration::from_secs(10), async move {
        let (read, mut write) = stream.into_split();
        let mut line = String::new();
        let mut reader = BufReader::new(read.take((MAX_REQUEST + 1) as u64));
        reader.read_line(&mut line).await?;
        let response = match decode_request(&line) {
            Ok(request) => {
                let (reply, rx) = oneshot::channel();
                tx.send(Incoming { request, reply })
                    .await
                    .context("Background process stopped")?;
                rx.await.context("Background process stopped")?
            }
            Err(error) => json!({"ok": false, "error": error.to_string()}),
        };
        write.write_all(format!("{response}\n").as_bytes()).await?;
        Ok::<_, anyhow::Error>(())
    })
    .await;
    if let Ok(Err(error)) = result {
        eprintln!("Control request: {error:#}");
    }
}

fn decode_request(line: &str) -> anyhow::Result<Request> {
    ensure!(
        line.len() <= MAX_REQUEST && line.ends_with('\n'),
        "Invalid or oversized request"
    );
    let request: Request = serde_json::from_str(line)?;
    // Serde's unit enum variants otherwise ignore extra object members.
    let received: Value = serde_json::from_str(line)?;
    let canonical = serde_json::to_value(&request)?;
    ensure!(
        received
            .as_object()
            .is_some_and(|object| object.keys().all(|key| canonical.get(key).is_some())),
        "Unknown control request field"
    );
    Ok(request)
}

pub(crate) async fn request(request: &Request) -> anyhow::Result<Value> {
    tokio::time::timeout(Duration::from_secs(12), async {
        let mut stream = UnixStream::connect(socket_path()).await?;
        let encoded = serde_json::to_vec(request)?;
        ensure!(
            encoded.len() < MAX_REQUEST,
            "Too many file paths in one request"
        );
        stream.write_all(&encoded).await?;
        stream.write_all(b"\n").await?;
        let mut line = String::new();
        BufReader::new(stream.take(1024 * 1024))
            .read_line(&mut line)
            .await?;
        ensure!(!line.is_empty(), "Empty control response");
        Ok::<Value, anyhow::Error>(serde_json::from_str(&line)?)
    })
    .await
    .context("LocalSend control request timed out")?
}

async fn ensure_running(args: &Args) -> anyhow::Result<Value> {
    if let Some(status) = running_status().await? {
        return Ok(status);
    }
    let directory = storage::dir();
    std::fs::create_dir_all(&directory)?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(directory.join("background.log"))?;
    let mut command = Process::new(std::env::current_exe()?);
    if let Some(alias) = &args.alias {
        command.args(["--alias", alias]);
    }
    if let Some(port) = args.port {
        command.args(["--port", &port.to_string()]);
    }
    if let Some(destination) = &args.destination {
        command.arg("--destination").arg(destination);
    }
    let mut child = command
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()?;
    for _ in 0..100 {
        if let Ok(status) = request(&Request::Status).await {
            return checked(status);
        }
        if let Some(code) = child.try_wait()? {
            // A concurrent starter may have acquired the lock first.
            tokio::time::sleep(Duration::from_millis(150)).await;
            if let Ok(status) = request(&Request::Status).await {
                return checked(status);
            }
            anyhow::bail!(
                "Background startup failed ({code}). Check {}. If the old terminal CLI is running, exit it first.",
                directory.join("background.log").display()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!(
        "Background startup timed out; check {}",
        directory.join("background.log").display()
    )
}

async fn running_status() -> anyhow::Result<Option<Value>> {
    match request(&Request::Status).await {
        Ok(status) => Ok(Some(checked(status)?)),
        Err(error)
            if error.downcast_ref::<io::Error>().is_some_and(|e| {
                matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                )
            }) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn checked(value: Value) -> anyhow::Result<Value> {
    ensure!(
        value["ok"] == true,
        "{}",
        value["error"]
            .as_str()
            .unwrap_or("LocalSend request failed")
    );
    Ok(value)
}

fn print_value(value: &Value, as_json: bool) {
    if as_json {
        println!("{value}");
    } else if value["running"] == false {
        println!("LocalSend is stopped.");
    } else {
        println!("{}", serde_json::to_string_pretty(value).unwrap());
    }
}

/// Explicit terminal discovery waits for its bounded scan. File-manager polls
/// use Devices directly and remain immediate, read-only snapshots.
async fn refresh_devices() -> anyhow::Result<Value> {
    checked(request(&Request::Refresh { timeout_ms: 4000 }).await?)?;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(4200);
    loop {
        let value = checked(request(&Request::Devices).await?)?;
        let checking = value["devices"]
            .as_array()
            .is_some_and(|devices| devices.iter().any(|device| device["checking"] == true));
        if (value["searching"] == false && !checking) || tokio::time::Instant::now() >= deadline {
            return Ok(value);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub(crate) async fn run(args: Args) -> anyhow::Result<()> {
    match &args.command {
        Some(Command::Daemon) => {
            let listener = Listener::bind()?;
            app::background::run(storage::Repository::load(&args)?, listener).await
        }
        Some(Command::Terminal) => {
            println!(
                "旧终端独立收发模式已停用，现统一使用后台。\n白名单：localsend pair/unpair DEVICE；取消：localsend cancel；链接：localsend web --help\n"
            );
            print_startup(&ensure_running(&args).await?, false);
            Ok(())
        }
        Some(Command::Status { json }) => {
            let value = running_status()
                .await?
                .unwrap_or_else(|| json!({"ok":true,"running":false}));
            print_value(&value, *json);
            Ok(())
        }
        Some(Command::Stop { force }) => {
            if running_status().await?.is_none() {
                println!("LocalSend is stopped.");
                return Ok(());
            }
            let response = checked(request(&Request::Stop { force: *force }).await?)?;
            for _ in 0..50 {
                if !socket_path().exists() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            ensure!(
                !socket_path().exists(),
                "Shutdown is still in progress; check status again"
            );
            print_value(&response, false);
            Ok(())
        }
        Some(Command::Accept { .. } | Command::Decline) => {
            let request_value = match args.command {
                Some(Command::Accept { pair: true }) => Request::AcceptPair,
                Some(Command::Accept { pair: false }) => Request::Accept,
                _ => Request::Decline,
            };
            print_value(&checked(request(&request_value).await?)?, false);
            Ok(())
        }
        Some(Command::Devices { json }) => {
            ensure_running(&args).await?;
            print_value(&refresh_devices().await?, *json);
            Ok(())
        }
        Some(Command::Pair { target } | Command::Unpair { target }) => {
            ensure_running(&args).await?;
            let request_value = if matches!(args.command, Some(Command::Pair { .. })) {
                // An explicit pairing action confirms the target's identity first.
                refresh_devices().await?;
                Request::Pair {
                    target: target.clone(),
                }
            } else {
                Request::Unpair {
                    target: target.clone(),
                }
            };
            let value = checked(request(&request_value).await?)?;
            println!("{}", value["message"].as_str().unwrap_or("白名单已更新"));
            Ok(())
        }
        Some(Command::Cancel) => {
            let value = checked(request(&Request::Cancel).await?)?;
            println!(
                "{}",
                value["message"]
                    .as_str()
                    .unwrap_or("已提交取消请求；后台继续运行")
            );
            Ok(())
        }
        Some(Command::Web { command }) => {
            ensure_running(&args).await?;
            let command = match command {
                WebCommand::Share { paths } => {
                    let paths = absolute_paths(paths)?;
                    validate_paths(&paths, false)?;
                    Request::WebShare { paths }
                }
                WebCommand::Receive => Request::WebReceive,
                WebCommand::Stop => Request::WebStop,
                WebCommand::Status => Request::WebStatus,
            };
            let value = checked(request(&command).await?)?;
            if value["web"]["mode"] == "off" {
                println!("链接模式已关闭，使用加密的 LocalSend 收发。");
            } else {
                println!("浏览器链接模式：HTTP（未加密，与原链接功能相同）");
                for url in value["web"]["urls"].as_array().into_iter().flatten() {
                    if let Some(url) = url.as_str() {
                        println!(" - {url}");
                    }
                }
                if let Some(qr) = value["web"]["qr"].as_str() {
                    println!("\n{qr}");
                }
                println!("\n关闭链接：localsend web stop");
            }
            Ok(())
        }
        Some(Command::Send { to, text, paths }) => {
            let text = text
                .as_deref()
                .map(app::background::read_text)
                .transpose()?;
            send(&args, to.as_deref(), paths.clone(), text).await
        }
        Some(Command::Start { json }) => {
            let status = ensure_running(&args).await?;
            print_startup(&status, *json);
            Ok(())
        }
        None if !args.files.is_empty() => send(&args, None, args.files.clone(), None).await,
        None => {
            print_startup(&ensure_running(&args).await?, false);
            Ok(())
        }
    }
}

fn absolute_paths(paths: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
    let current = std::env::current_dir()?;
    Ok(paths
        .iter()
        .map(|path| {
            if path.is_absolute() {
                path.clone()
            } else {
                current.join(path)
            }
        })
        .collect())
}

fn print_startup(status: &Value, as_json: bool) {
    if as_json {
        print_value(status, true);
    } else {
        let color = io::stdout().is_terminal()
            && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty());
        print!(
            "{}",
            crate::banner::render_background(status, &storage::dir(), color)
        );
    }
}

async fn send(
    args: &Args,
    target: Option<&str>,
    paths: Vec<PathBuf>,
    text: Option<String>,
) -> anyhow::Result<()> {
    let current = std::env::current_dir()?;
    let paths: Vec<_> = paths
        .into_iter()
        .map(|p| if p.is_absolute() { p } else { current.join(p) })
        .collect();
    validate_paths(&paths, text.is_some())?;
    ensure_running(args).await?;
    let target = match target {
        Some(target) => target.to_string(),
        None => {
            let devices = refresh_devices().await?;
            let devices = devices["devices"]
                .as_array()
                .context("Invalid device list")?;
            ensure!(
                !devices.is_empty(),
                "No devices found; open LocalSend on your phone and try again"
            );
            for (index, device) in devices.iter().enumerate() {
                println!(
                    "{}: {}",
                    index + 1,
                    device["alias"].as_str().unwrap_or("Unknown")
                );
            }
            print!("Select device: ");
            io::stdout().flush()?;
            let mut line = String::new();
            io::stdin().read_line(&mut line)?;
            let index: usize = line.trim().parse().context("Enter a device number")?;
            let device = index
                .checked_sub(1)
                .and_then(|i| devices.get(i))
                .context("Invalid device number")?;
            device["fingerprint"]
                .as_str()
                .context("Invalid device identity")?
                .to_string()
        }
    };
    let response = checked(
        request(&Request::Send {
            target,
            paths,
            text,
        })
        .await?,
    )?;
    print_value(&response, false);
    Ok(())
}

pub(crate) fn validate_paths(paths: &[PathBuf], has_text: bool) -> anyhow::Result<()> {
    ensure!(
        has_text == paths.is_empty(),
        "Provide files/directories or text, not both"
    );
    ensure!(
        paths.len() <= 1024,
        "Too many selected items (maximum 1024)"
    );
    for path in paths {
        ensure!(
            path.is_absolute(),
            "Use an absolute file path: {}",
            path.display()
        );
        ensure!(
            path.is_file() || path.is_dir(),
            "Not a file or directory: {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_incomplete_and_oversized_control_messages() {
        assert!(decode_request("{\"command\":\"status\"}").is_err());
        assert!(decode_request(&format!("{}\n", " ".repeat(MAX_REQUEST))).is_err());
        assert!(decode_request("{\"command\":\"status\",\"extra\":true}\n").is_err());
        assert!(matches!(
            decode_request("{\"command\":\"status\"}\n").unwrap(),
            Request::Status
        ));
    }

    #[test]
    fn rejects_missing_relative_and_mixed_send_payloads() {
        assert!(validate_paths(&[], false).is_err());
        assert!(validate_paths(&[PathBuf::from("relative")], false).is_err());
        assert!(validate_paths(&[PathBuf::from("/tmp")], true).is_err());
        assert!(validate_paths(&[], true).is_ok());
        assert!(validate_paths(&[PathBuf::from("/tmp")], false).is_ok());
    }
}
