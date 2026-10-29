use crate::backend::{Device, Job, Reply};
use futures_util::StreamExt;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

#[derive(Default)]
struct State {
    paths: Vec<PathBuf>,
    devices: Vec<Device>,
    order: HashMap<String, usize>,
    rendered: Vec<Row>,
    loading: bool,
    sending: bool,
    refresh_pending: bool,
    closed: bool,
}

#[derive(PartialEq, Eq)]
struct Row {
    fingerprint: String,
    label: String,
    enabled: bool,
}

fn single_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

impl State {
    fn rows(&mut self) -> Vec<Row> {
        for device in &self.devices {
            let next = self.order.len();
            self.order.entry(device.fingerprint.clone()).or_insert(next);
        }
        let mut devices: Vec<_> = self.devices.iter().collect();
        devices.sort_by_key(|device| self.order[&device.fingerprint]);
        devices
            .into_iter()
            .map(|device| {
                let mut alias = single_line(&device.alias);
                if alias.trim().is_empty() {
                    alias = "未命名设备".into();
                }
                if self
                    .devices
                    .iter()
                    .filter(|other| other.alias == device.alias)
                    .count()
                    > 1
                {
                    let suffix: String = device
                        .fingerprint
                        .chars()
                        .rev()
                        .take(6)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    alias.push_str(&format!(" · {suffix}"));
                }
                if !device.online {
                    alias.push_str("（未连接）");
                }
                Row {
                    fingerprint: device.fingerprint.clone(),
                    label: alias,
                    enabled: !self.paths.is_empty() && device.online && !self.sending,
                }
            })
            .collect()
    }
}

struct SendWindow {
    window: gtk::ApplicationWindow,
    selection: gtk::Label,
    rows: gtk::Box,
    hint: gtk::Label,
    refresh: gtk::Button,
    choices: gtk::Box,
    cancel: gio::Cancellable,
    choosing: Cell<bool>,
    state: RefCell<State>,
    requests: Sender<Job>,
    next_poll: Cell<Instant>,
}

impl SendWindow {
    fn new(
        app: &gtk::Application,
        requests: Sender<Job>,
        mut replies: futures_channel::mpsc::UnboundedReceiver<Reply>,
    ) -> Rc<Self> {
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("LocalSend · 发送")
            .default_width(420)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
        content.set_margin_top(20);
        content.set_margin_bottom(20);
        content.set_margin_start(20);
        content.set_margin_end(20);
        let choices = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let files = gtk::Button::with_label("选择文件");
        let folders = gtk::Button::with_label("选择文件夹");
        choices.append(&files);
        choices.append(&folders);
        content.append(&choices);
        let selection = gtk::Label::builder()
            .label("尚未选择文件")
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(42)
            .build();
        content.append(&selection);
        content.append(&gtk::Label::builder().label("发送到").xalign(0.0).build());
        let rows = gtk::Box::new(gtk::Orientation::Vertical, 5);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .max_content_height(360)
            .propagate_natural_height(true)
            .child(&rows)
            .build();
        content.append(&scroll);
        let hint = gtk::Label::builder()
            .label("正在查找设备…")
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(42)
            .build();
        content.append(&hint);
        let refresh = gtk::Button::with_label("刷新设备列表");
        content.append(&refresh);
        window.set_child(Some(&content));
        let this = Rc::new(Self {
            window,
            selection,
            rows,
            hint,
            refresh,
            choices,
            cancel: gio::Cancellable::new(),
            choosing: Cell::new(false),
            state: RefCell::new(State::default()),
            requests,
            next_poll: Cell::new(Instant::now()),
        });
        for (button, folder) in [(files, false), (folders, true)] {
            let weak = Rc::downgrade(&this);
            button.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.choose(folder);
                }
            });
        }
        let weak = Rc::downgrade(&this);
        this.refresh.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.load(true);
            }
        });
        let weak = Rc::downgrade(&this);
        this.window.connect_close_request(move |_| {
            if let Some(this) = weak.upgrade() {
                this.close();
            }
            glib::Propagation::Proceed
        });
        this.state.borrow_mut().loading = true;
        if this.requests.send(Job::Open).is_err() {
            this.backend_failed();
        }
        this.next_poll
            .set(Instant::now() + Duration::from_millis(400));
        let weak = Rc::downgrade(&this);
        glib::spawn_future_local(async move {
            while let Some(reply) = replies.next().await {
                let Some(this) = weak.upgrade() else {
                    break;
                };
                if this.state.borrow().closed {
                    break;
                }
                this.received(reply);
            }
        });
        let ticking = this.clone();
        glib::timeout_add_local(Duration::from_millis(400), move || {
            if ticking.state.borrow().closed {
                return glib::ControlFlow::Break;
            }
            if Instant::now() >= ticking.next_poll.get() {
                ticking.load(false);
                ticking
                    .next_poll
                    .set(Instant::now() + Duration::from_millis(400));
            }
            glib::ControlFlow::Continue
        });
        this
    }

    fn choose(self: &Rc<Self>, folder: bool) {
        if self.state.borrow().closed || self.state.borrow().sending || self.choosing.replace(true)
        {
            return;
        }
        self.choices.set_sensitive(false);
        let dialog = gtk::FileDialog::builder()
            .title(if folder {
                "选择要发送的文件夹"
            } else {
                "选择要发送的文件"
            })
            .build();
        let weak = Rc::downgrade(self);
        let selected = move |result: Result<gio::ListModel, glib::Error>| {
            let Some(this) = weak.upgrade() else {
                return;
            };
            this.choosing.set(false);
            if this.state.borrow().closed {
                return;
            }
            this.choices.set_sensitive(!this.state.borrow().sending);
            match result {
                Ok(model) => {
                    let paths = (0..model.n_items())
                        .map(|index| model.item(index)?.downcast::<gio::File>().ok()?.path())
                        .collect::<Option<Vec<_>>>();
                    if let Some(paths) = paths {
                        this.select_paths(paths);
                    } else {
                        this.hint.set_label("请选择本机文件或文件夹。");
                    }
                }
                Err(error)
                    if error.matches(gtk::DialogError::Dismissed)
                        || error.matches(gtk::DialogError::Cancelled) => {}
                Err(error) => this.hint.set_label(error.message()),
            }
        };
        if folder {
            dialog.select_multiple_folders(Some(&self.window), Some(&self.cancel), selected);
        } else {
            dialog.open_multiple(Some(&self.window), Some(&self.cancel), selected);
        }
    }

    fn select_paths(self: &Rc<Self>, paths: Vec<PathBuf>) {
        let mut seen = HashSet::new();
        let paths: Vec<_> = paths
            .into_iter()
            .filter(|path| seen.insert(path.clone()))
            .collect();
        let label = paths
            .iter()
            .map(|path| {
                single_line(
                    &path
                        .file_name()
                        .unwrap_or(path.as_os_str())
                        .to_string_lossy(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        self.selection.set_label(if paths.is_empty() {
            "尚未选择文件"
        } else {
            &label
        });
        self.state.borrow_mut().paths = paths;
        self.render();
    }

    fn load(self: &Rc<Self>, refresh: bool) {
        let mut state = self.state.borrow_mut();
        if state.closed || state.sending {
            return;
        }
        if state.loading {
            state.refresh_pending |= refresh;
            return;
        }
        let refresh = refresh || std::mem::take(&mut state.refresh_pending);
        state.loading = true;
        drop(state);
        if refresh {
            self.hint.set_label("正在查找设备…");
        }
        if self
            .requests
            .send(if refresh { Job::Refresh } else { Job::Poll })
            .is_err()
        {
            self.backend_failed();
        }
    }

    fn backend_failed(self: &Rc<Self>) {
        let mut state = self.state.borrow_mut();
        state.loading = false;
        state.devices.clear();
        drop(state);
        self.hint
            .set_label("后台已停止或暂时无法连接，请从顶栏重新启动。");
        self.render();
    }

    fn received(self: &Rc<Self>, reply: Reply) {
        if self.state.borrow().closed {
            return;
        }
        match reply {
            Reply::Devices(result) => {
                self.state.borrow_mut().loading = false;
                match result {
                    Ok(devices) => {
                        if !self.state.borrow().sending {
                            self.hint.set_label(if devices.iter().any(|d| d.online) {
                                "选择文件后，点击在线设备发送。"
                            } else {
                                "请保持对方 LocalSend 打开，并连接同一局域网。"
                            });
                        }
                        self.state.borrow_mut().devices = devices;
                    }
                    Err(_) => self.backend_failed(),
                }
                self.render();
            }
            Reply::Sent(result) => {
                self.state.borrow_mut().sending = false;
                match result {
                    Ok(()) => self.window.close(),
                    Err(error) => {
                        self.hint.set_label(&single_line(&error));
                        self.refresh.set_sensitive(true);
                        self.choices.set_sensitive(!self.choosing.get());
                        // Keep errors visible until an explicit refresh or selection.
                        self.next_poll.set(Instant::now() + Duration::from_secs(5));
                        self.render();
                    }
                }
            }
        }
    }

    fn render(self: &Rc<Self>) {
        let mut state = self.state.borrow_mut();
        let rows = state.rows();
        if state.rendered == rows {
            return;
        }
        while let Some(child) = self.rows.first_child() {
            self.rows.remove(&child);
        }
        for row in &rows {
            let button = gtk::Button::with_label(&row.label);
            button.set_sensitive(row.enabled);
            let weak = Rc::downgrade(self);
            let target = row.fingerprint.clone();
            button.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.send(&target);
                }
            });
            self.rows.append(&button);
        }
        state.rendered = rows;
    }

    fn send(self: &Rc<Self>, target: &str) {
        let mut state = self.state.borrow_mut();
        if state.closed
            || state.sending
            || state.paths.is_empty()
            || !state
                .devices
                .iter()
                .any(|device| device.fingerprint == target && device.online)
        {
            return;
        }
        state.sending = true;
        let paths = state.paths.clone();
        drop(state);
        self.refresh.set_sensitive(false);
        self.choices.set_sensitive(false);
        self.hint.set_label("正在提交发送任务…");
        self.render();
        if self
            .requests
            .send(Job::Send {
                target: target.into(),
                paths,
            })
            .is_err()
        {
            self.received(Reply::Sent(Err("后台连接已关闭，请重试。".into())));
        }
    }

    fn close(&self) {
        let mut state = self.state.borrow_mut();
        if state.closed {
            return;
        }
        state.closed = true;
        drop(state);
        self.cancel.cancel();
        let _ = self.requests.send(Job::Close);
    }
}

pub fn run() -> anyhow::Result<()> {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--version")) {
        println!("localsend-picker {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let worker = crate::backend::start(crate::backend::socket_path()?);
    let requests = worker.requests.clone();
    let replies = RefCell::new(Some(worker.replies));
    let app = gtk::Application::builder()
        .application_id("local.localsend.SendPicker")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(move |app| {
        if let Some(replies) = replies.borrow_mut().take() {
            let window = SendWindow::new(app, requests.clone(), replies);
            window.window.present();
            glib::idle_add_local_once(move || window.choose(false));
        } else if let Some(window) = app.active_window() {
            window.present();
        }
    });
    let code = app.run_with_args(&["localsend-picker"]);
    let _ = worker.requests.send(Job::Close);
    let _ = worker.thread.join();
    std::process::exit(code.into());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_order_duplicate_names_and_enabled_state() {
        let device = |fingerprint: &str, online| Device {
            fingerprint: fingerprint.into(),
            alias: "手机\n".into(),
            online,
            paired: true,
        };
        let mut state = State {
            paths: vec!["/tmp/file".into()],
            devices: vec![device("ABCDEF1", true), device("ABCDEF2", false)],
            ..State::default()
        };
        let rows = state.rows();
        assert_eq!(rows[0].label, "手机  · BCDEF1");
        assert!(rows[0].enabled);
        assert!(!rows[1].enabled);
        state.devices.reverse();
        assert_eq!(state.rows()[0].fingerprint, "ABCDEF1");
        state.sending = true;
        assert!(state.rows().iter().all(|row| !row.enabled));
    }

    // Run explicitly under Broadway: normal unit tests do not require a display.
    #[test]
    #[ignore = "requires an isolated GTK display"]
    fn gtk_window_selection_errors_and_close() {
        gtk::init().unwrap();
        let app = gtk::Application::builder()
            .application_id("local.localsend.PickerTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let (requests, jobs) = std::sync::mpsc::channel();
        let (results, replies) = futures_channel::mpsc::unbounded();
        let window = SendWindow::new(&app, requests, replies);
        window.window.present();
        assert!(matches!(jobs.recv().unwrap(), Job::Open));
        window
            .next_poll
            .set(Instant::now() + Duration::from_secs(60));
        let devices = vec![
            Device {
                fingerprint: "LIVE".into(),
                alias: "手机".into(),
                online: true,
                paired: false,
            },
            Device {
                fingerprint: "OFF".into(),
                alias: "平板".into(),
                online: false,
                paired: true,
            },
        ];
        window.received(Reply::Devices(Ok(devices.clone())));
        // Exercise both real GTK asynchronous chooser paths and cancellation.
        // A dismissal must preserve the previous selection and unlock controls.
        for folder in [false, true] {
            let (chooser_requests, _) = std::sync::mpsc::channel();
            let (_chooser_results, chooser_replies) = futures_channel::mpsc::unbounded();
            let chooser = SendWindow::new(&app, chooser_requests, chooser_replies);
            chooser.window.present();
            chooser.choose(folder);
            assert!(chooser.choosing.get());
            let mapped = Instant::now() + Duration::from_millis(150);
            while Instant::now() < mapped {
                glib::MainContext::default().iteration(false);
                std::thread::sleep(Duration::from_millis(5));
            }
            chooser.cancel.cancel();
            let deadline = Instant::now() + Duration::from_secs(3);
            while chooser.choosing.get() && Instant::now() < deadline {
                glib::MainContext::default().iteration(false);
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(!chooser.choosing.get());
            assert!(chooser.choices.is_sensitive());
            assert!(chooser.state.borrow().paths.is_empty());
            chooser.window.close();
        }
        window.select_paths(vec![
            "/tmp/中文 folder/$(touch no).txt".into(),
            "/tmp/folder".into(),
            "/tmp/folder".into(),
        ]);
        let first = window.rows.first_child().unwrap();
        assert!(first.is_sensitive());
        assert!(!first.next_sibling().unwrap().is_sensitive());
        window.render();
        assert_eq!(
            first,
            window.rows.first_child().unwrap(),
            "unchanged polls must preserve widgets and focus"
        );
        window.load(true);
        assert!(matches!(jobs.recv().unwrap(), Job::Refresh));
        window.received(Reply::Devices(Ok(devices.clone())));
        window.send("OFF");
        assert!(jobs.try_recv().is_err());
        window.send("LIVE");
        match jobs.recv().unwrap() {
            Job::Send { target, paths } => {
                assert_eq!(target, "LIVE");
                assert_eq!(
                    paths,
                    vec![
                        PathBuf::from("/tmp/中文 folder/$(touch no).txt"),
                        "/tmp/folder".into()
                    ]
                );
            }
            _ => panic!("expected send"),
        }
        window.received(Reply::Sent(Err("queue full".into())));
        assert_eq!(window.hint.label(), "queue full");
        assert!(window.rows.first_child().unwrap().is_sensitive());
        window.received(Reply::Devices(Err("connection refused".into())));
        assert!(window.rows.first_child().is_none());
        assert!(window.hint.label().contains("后台已停止"));
        window.received(Reply::Devices(Ok(devices)));
        window.send("LIVE");
        assert!(matches!(jobs.recv().unwrap(), Job::Send { .. }));
        window.received(Reply::Sent(Ok(())));
        assert!(window.state.borrow().closed);
        assert!(matches!(jobs.recv().unwrap(), Job::Close));
        window.close();
        assert!(jobs.try_recv().is_err());
        drop(results);
        window.window.destroy();
        let context = glib::MainContext::default();
        while context.pending() {
            context.iteration(false);
        }
    }
}
