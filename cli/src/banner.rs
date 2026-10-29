use crate::storage::Repository;
use crossterm::style::Stylize;
use localsend::http::server::ServerHandle;
use serde_json::Value;
use std::net::SocketAddr;
use std::path::Path;

#[rustfmt::skip]
const LOGO: [&str; 4] = [
    " ▄▀ ▀ ▀▄ ",
    "▄ ▄███▄ ▄",
    "▀ ▀███▀ ▀",
    " ▀▄ ▄ ▄▀ ",
];

fn green(text: &str, color: bool) -> String {
    if color {
        text.green().to_string()
    } else {
        text.to_string()
    }
}

fn logo(color: bool) -> String {
    LOGO.iter()
        .enumerate()
        .map(|(i, line)| {
            let right = match i {
                1 => "  LocalSend CLI",
                2 => concat!("  v", env!("CARGO_PKG_VERSION")),
                _ => "",
            };
            format!("{}{right}", green(line, color))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn render(storage: &Repository, server: &ServerHandle) -> String {
    let logo = logo(true);

    let listening = match listening_lines(&server.local_addresses(), "https") {
        lines if lines.is_empty() => " - (no network interface found)".to_string(),
        lines => lines.join("\n"),
    };

    format!(
        "{logo}\n\n{} {}\n{} {}\n{} {}\n{} {}\n{}\n{listening}\n\nReady to accept requests.\n\n",
        "Alias:".green(),
        storage.identity.alias,
        "Port:".green(),
        storage.identity.port,
        "Destination:".green(),
        storage.destination.display(),
        "Config:".green(),
        storage.dir.display(),
        "Listening on:".green(),
    )
}

/// Render a read-only backend snapshot; this never loads configuration,
/// starts discovery, or waits for transfers to finish.
pub fn render_background(status: &Value, config: &Path, color: bool) -> String {
    let value = |key: &str| match status.get(key) {
        Some(Value::String(text)) => crate::sanitize::single_line(text),
        Some(Value::Number(number)) => number.to_string(),
        _ => "—".to_string(),
    };
    let line = |name: &str, value: &str| format!("{} {value}\n", green(&format!("{name}:"), color));
    let mut output = format!("{}\n\n", logo(color));
    output += &line("设备名称", &value("alias"));
    output += &line("运行模式", "后台运行 · 关闭终端后继续运行");
    output += &line("进程编号", &value("pid"));
    let uptime = status["uptime_seconds"]
        .as_u64()
        .map(|seconds| crate::util::format_duration(std::time::Duration::from_secs(seconds)))
        .unwrap_or_else(|| "—".into());
    output += &line("运行时长", &uptime);
    output += &line("服务端口", &value("port"));
    output += &line("接收目录", &value("destination"));
    output += &line(
        "配置目录",
        &crate::sanitize::single_line(&config.display().to_string()),
    );
    output += &line(
        "后台日志",
        &crate::sanitize::single_line(&config.join("background.log").display().to_string()),
    );
    output += &line("白名单", &format!("{} 台设备", value("paired_devices")));
    let transfer = |key: &str| {
        let item = &status[key];
        match item["alias"].as_str() {
            Some(alias) => format!(
                "{}  {} / {}",
                crate::sanitize::single_line(alias),
                crate::util::format_bytes(item["bytes"].as_u64().unwrap_or(0)),
                crate::util::format_bytes(item["total_bytes"].as_u64().unwrap_or(0))
            ),
            None => "空闲".to_string(),
        }
    };
    output += "\n";
    output += &line("发送任务", &transfer("sending"));
    output += &line("接收任务", &transfer("receiving"));
    output += &line(
        "等待确认",
        &status["pending_sender"]
            .as_str()
            .map(crate::sanitize::single_line)
            .unwrap_or_else(|| "无".into()),
    );
    output += &line(
        "发送队列",
        &format!("{} 项", status["queued"].as_u64().unwrap_or(0)),
    );
    let discovery = if status["discovery"]["menu_sessions"].as_u64().unwrap_or(0) > 0 {
        "菜单已展开，按需确认设备在线状态"
    } else if status["discovery"]["searching"] == true {
        "正在公告或查找设备，结束后恢复被动监听"
    } else {
        "被动监听；展开设备菜单时按需确认"
    };
    output += &line("设备发现", discovery);
    output += &line("接收规则", "白名单自动接收；其他设备 4 秒内确认");
    output += &format!("\n{}\n", green("监听地址:", color));
    let addresses: Vec<SocketAddr> = status["listening_addresses"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|address| address.as_str()?.parse().ok())
        .collect();
    let scheme = if status["scheme"] == "http" {
        "http"
    } else {
        "https"
    };
    let listening = listening_lines(&addresses, scheme);
    if listening.is_empty() {
        output += " - 暂无可用地址\n";
    } else {
        output += &format!("{}\n", listening.join("\n"));
    }
    output += &format!("\n{}\n", green("后台已就绪，可以关闭当前终端。", color));
    output +=
        "查看状态  localsend status\n查找设备  localsend devices\n停止后台  localsend stop\n\n";
    output
}

/// One line per IPv4 address, but only one per IPv6 scope (global,
/// unique-local) with a `(+N more)` suffix for the rest: interfaces usually carry
/// several equivalent IPv6 addresses (e.g. temporary privacy addresses),
/// which would flood the banner.
///
/// IPv6 addresses render bracketed ("[::1]:53317"), as a URL needs them.
/// Relies on the addresses being sorted, so that each scope is contiguous.
pub fn listening_lines(addresses: &[SocketAddr], scheme: &str) -> Vec<String> {
    let mut lines: Vec<(String, usize)> = Vec::new();
    let mut current_scope: Option<bool> = None;
    for address in addresses {
        match address {
            SocketAddr::V4(_) => lines.push((format!(" - {scheme}://{address}"), 0)),
            SocketAddr::V6(v6) => {
                let scope = v6.ip().is_unique_local();
                match current_scope == Some(scope) {
                    true => lines.last_mut().unwrap().1 += 1,
                    false => {
                        current_scope = Some(scope);
                        lines.push((format!(" - {scheme}://{address}"), 0));
                    }
                }
            }
        }
    }

    lines
        .into_iter()
        .map(|(line, hidden)| match hidden {
            0 => line,
            hidden => format!("{line} (+{hidden} more)"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_snapshot_shows_tasks_and_never_interprets_device_control_characters() {
        let status = serde_json::json!({"alias":"电脑\u{1b}[31m\n", "pid":12,"port":53318,
            "destination":"/tmp/LocalSend","uptime_seconds":65,"paired_devices":2,"queued":1,
            "sending":{"alias":"iPad\u{1b}[2J\n","bytes":500,"total_bytes":1000},
            "receiving":null,"pending_sender":"手机\n请求", "discovery":{"searching":false},
            "listening_addresses":["192.168.1.2:53318","[fd44::1]:53318"]});
        let text = render_background(&status, Path::new("/tmp/配置"), false);
        assert!(!text.contains('\u{1b}'));
        assert!(text.contains("运行时长: 1m 5s"));
        assert!(text.contains("白名单: 2 台设备"));
        assert!(text.contains("发送任务: iPad[2J   500 B / 1.0 KB"));
        assert!(text.contains("接收任务: 空闲"));
        assert!(text.contains("等待确认: 手机 请求"));
        assert!(text.contains("https://[fd44::1]:53318"));
        assert!(text.contains("后台日志: /tmp/配置/background.log"));
    }

    #[test]
    fn background_color_is_optional_and_older_snapshots_remain_readable() {
        let status = serde_json::json!({"alias":"电脑","pid":12,"port":53318,"destination":"/tmp"});
        let plain = render_background(&status, Path::new("/tmp"), false);
        let colored = render_background(&status, Path::new("/tmp"), true);
        assert!(!plain.contains('\u{1b}'));
        assert!(colored.contains('\u{1b}'));
        assert!(plain.contains("运行时长: —"));
        assert!(plain.contains("暂无可用地址"));
        assert!(plain.contains("LocalSend CLI"));
    }

    fn address(s: &str) -> SocketAddr {
        SocketAddr::new(s.parse().unwrap(), 53317)
    }

    #[test]
    fn test_every_ipv4_gets_a_line() {
        let lines = listening_lines(&[address("10.0.0.1"), address("192.168.0.1")], "https");
        assert_eq!(
            lines,
            [" - https://10.0.0.1:53317", " - https://192.168.0.1:53317"]
        );
    }

    #[test]
    fn test_scheme_is_configurable() {
        let lines = listening_lines(&[address("10.0.0.1")], "http");
        assert_eq!(lines, [" - http://10.0.0.1:53317"]);
    }

    #[test]
    fn test_ipv6_collapses_per_scope() {
        let lines = listening_lines(
            &[
                address("192.168.0.1"),
                address("2a02::1"),
                address("2a02::2"),
                address("2a02::3"),
                address("fd44::1"),
                address("fd44::2"),
            ],
            "https",
        );
        assert_eq!(
            lines,
            [
                " - https://192.168.0.1:53317",
                " - https://[2a02::1]:53317 (+2 more)",
                " - https://[fd44::1]:53317 (+1 more)",
            ]
        );
    }

    #[test]
    fn test_single_ipv6_has_no_suffix() {
        let lines = listening_lines(&[address("2a02::1")], "https");
        assert_eq!(lines, [" - https://[2a02::1]:53317"]);
    }
}
