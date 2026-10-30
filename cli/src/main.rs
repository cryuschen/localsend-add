// Modified by localsend-add contributors, 2026. See NOTICE and LICENSE.
mod app;
mod banner;
#[cfg(unix)]
mod control;
mod device_list;
mod picker;
mod sanitize;
mod send_task;
mod slots;
mod storage;
mod ui;
mod util;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

pub(crate) const LOCAL_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-local-unified.5");

#[derive(Subcommand)]
pub enum WebCommand {
    /// Share files/directories with browsers (plain HTTP, without encryption)
    Share {
        #[arg(value_name = "PATH", required = true, num_args = 1..)]
        paths: Vec<PathBuf>,
    },
    /// Let browsers upload files (plain HTTP, without encryption)
    Receive,
    /// Show current web links and QR code
    Status,
    /// Close web links and restore encrypted LocalSend receiving
    Stop,
}

#[derive(Subcommand)]
pub enum Command {
    /// Start the background process on demand (no login autostart)
    Start {
        /// Print the startup status as JSON instead of the terminal banner
        #[arg(long)]
        json: bool,
    },
    /// Show the background process and transfer status
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Stop when idle; use --force to cancel active or queued transfers
    Stop {
        #[arg(long)]
        force: bool,
    },
    /// List discovered and paired devices, starting the process if needed
    Devices {
        #[arg(long)]
        json: bool,
    },
    /// Accept a pending incoming transfer
    Accept {
        /// Also add this sender to the trusted devices
        #[arg(long)]
        pair: bool,
    },
    /// Decline a pending incoming transfer
    Decline,
    /// Compatibility alias for the unified background service
    #[command(hide = true)]
    Terminal,
    /// Trust a discovered device (exact alias, fingerprint, or IP)
    Pair { target: String },
    /// Remove a trusted device (exact alias or fingerprint)
    Unpair { target: String },
    /// Cancel current transfers and queued sends; keep the service running
    Cancel,
    /// Browser sharing and receiving, using the existing web protocol
    Web {
        #[command(subcommand)]
        command: WebCommand,
    },
    #[command(hide = true)]
    Daemon,
    /// Send one or more files or directories, or a text message
    Send {
        /// Destination device: an exact alias or IP address
        #[arg(long, value_name = "TARGET")]
        to: Option<String>,

        /// Send this text as a message instead of files; `-` reads it from stdin
        #[arg(long, value_name = "TEXT", requires = "to", conflicts_with = "paths")]
        text: Option<String>,

        /// Files or directories to send (directories are collected recursively)
        #[arg(value_name = "PATH", required_unless_present = "text", num_args = 1..)]
        paths: Vec<PathBuf>,
    },
}

/// LocalSend CLI
#[derive(Parser)]
#[command(name = "localsend-cli", version = LOCAL_VERSION, about, after_help = HELP_SECTIONS)]
pub struct Args {
    /// Device name shown to other devices [default: config.toml, else the hostname]
    #[arg(long, env = "LOCALSEND_ALIAS")]
    pub alias: Option<String>,

    /// Port of the HTTP server [default: config.toml, else 53317]
    #[arg(long, env = "LOCALSEND_PORT")]
    pub port: Option<u16>,

    /// Directory where received files are saved [default: config.toml, else the Downloads folder]
    #[arg(long, env = "LOCALSEND_DESTINATION")]
    pub destination: Option<PathBuf>,

    /// Files/directories to send using the unified background service
    #[arg(short = 'f', long = "file", value_name = "PATH")]
    pub files: Vec<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

const HELP_SECTIONS: &str = "Unified service:\n  \
                             localsend / localsend start   Start or reuse the same background service\n  \
                             localsend pair DEVICE         Add a trusted device\n  \
                             localsend unpair DEVICE       Remove a trusted device\n  \
                             localsend accept --pair       Accept and trust the pending sender\n  \
                             localsend cancel              Cancel transfers, keep the service running\n  \
                             localsend web share PATH...   Share via browser (HTTP)\n  \
                             localsend web receive         Receive via browser (HTTP)\n  \
                             localsend web stop            Restore encrypted receiving\n  \
                             1-9 in the open Files LocalSend menu selects that device\n\
                             \nEnvironment Variables:\n  \
                             XDG_CONFIG_HOME, LOCALSEND_ALIAS, LOCALSEND_PORT, LOCALSEND_DESTINATION";

#[cfg(not(unix))]
const _LEGACY_HELP_SECTIONS: &str = "Events:\n  \
                             D      Discovery\n  \
                             S      Sending files\n  \
                             R      Receiving files\n\
                             \nHotkeys:\n  \
                             1-9    Send files to the device with that number\n  \
                             D      Show the paired and discovered devices\n  \
                             W+S    Toggle share via link: serve files to browsers over plain HTTP\n  \
                             W+R    Toggle receive via link: let browsers upload files over plain HTTP\n  \
                             Y/N/P  Accept / Decline / Accept-and-Pair an incoming request\n  \
                             Ctrl+C Cancel the current transfer or request, or quit when idle\n \
                             \nEnvironment Variables:\n  \
                             XDG_CONFIG_HOME, LOCALSEND_ALIAS, LOCALSEND_PORT, LOCALSEND_DESTINATION";

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    #[cfg(unix)]
    return runtime.block_on(control::run(args));
    #[cfg(not(unix))]
    runtime.block_on(app::run(args))
}

#[cfg(test)]
mod tests {
    use super::{Args, Command, WebCommand};
    use clap::Parser;
    use std::path::PathBuf;

    #[test]
    fn startup_json_is_explicit_and_default_start_is_human_readable() {
        let args = Args::try_parse_from(["localsend-cli", "start", "--json"]).unwrap();
        assert!(matches!(args.command, Some(Command::Start { json: true })));
        let args = Args::try_parse_from(["localsend-cli", "start"]).unwrap();
        assert!(matches!(args.command, Some(Command::Start { json: false })));
    }

    #[test]
    fn management_commands_replace_the_legacy_terminal_actions() {
        assert!(
            matches!(Args::try_parse_from(["localsend","pair","平板"]).unwrap().command,Some(Command::Pair{target}) if target == "平板")
        );
        assert!(
            matches!(Args::try_parse_from(["localsend","unpair","FP"]).unwrap().command,Some(Command::Unpair{target}) if target == "FP")
        );
        assert!(matches!(
            Args::try_parse_from(["localsend", "accept", "--pair"])
                .unwrap()
                .command,
            Some(Command::Accept { pair: true })
        ));
        assert!(matches!(
            Args::try_parse_from(["localsend", "web", "share", "--", "文件夹"])
                .unwrap()
                .command,
            Some(Command::Web {
                command: WebCommand::Share { .. }
            })
        ));
        assert!(Args::try_parse_from(["localsend", "web", "share"]).is_err());
    }

    #[test]
    fn accepts_mixed_send_paths() {
        let args = Args::try_parse_from(["localsend-cli", "send", "one.txt", "two.txt", "backup"])
            .unwrap();

        let Some(Command::Send { to, paths, .. }) = args.command else {
            panic!("expected the send command");
        };
        assert_eq!(to, None);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("one.txt"),
                PathBuf::from("two.txt"),
                PathBuf::from("backup"),
            ]
        );
    }

    #[test]
    fn accepts_a_destination_alias() {
        let args =
            Args::try_parse_from(["localsend-cli", "send", "--to", "Cute Tomato", "one.txt"])
                .unwrap();

        let Some(Command::Send { to, paths, .. }) = args.command else {
            panic!("expected the send command");
        };
        assert_eq!(to.as_deref(), Some("Cute Tomato"));
        assert_eq!(paths, vec![PathBuf::from("one.txt")]);
    }

    #[test]
    fn accepts_a_destination_ip() {
        let args =
            Args::try_parse_from(["localsend-cli", "send", "--to", "192.168.27.26", "one.txt"])
                .unwrap();

        let Some(Command::Send { to, paths, .. }) = args.command else {
            panic!("expected the send command");
        };
        assert_eq!(to.as_deref(), Some("192.168.27.26"));
        assert_eq!(paths, vec![PathBuf::from("one.txt")]);
    }

    #[test]
    fn requires_at_least_one_send_path() {
        assert!(Args::try_parse_from(["localsend-cli", "send"]).is_err());
    }

    #[test]
    fn accepts_a_text_message() {
        let args =
            Args::try_parse_from(["localsend-cli", "send", "--to", "Phone", "--text", "hello"])
                .unwrap();

        let Some(Command::Send { to, text, paths }) = args.command else {
            panic!("expected the send command");
        };
        assert_eq!(to.as_deref(), Some("Phone"));
        assert_eq!(text.as_deref(), Some("hello"));
        assert!(paths.is_empty());
    }

    #[test]
    fn rejects_text_together_with_paths() {
        assert!(
            Args::try_parse_from([
                "localsend-cli",
                "send",
                "--to",
                "Phone",
                "--text",
                "hello",
                "one.txt"
            ])
            .is_err()
        );
    }

    #[test]
    fn requires_a_destination_for_text() {
        assert!(Args::try_parse_from(["localsend-cli", "send", "--text", "hello"]).is_err());
    }

    #[test]
    fn accepts_interactive_mode_without_a_command() {
        let args = Args::try_parse_from(["localsend-cli"]).unwrap();

        assert!(args.command.is_none());
    }
}
