# LocalSend 常驻圆环

适用于本机 GNOME Shell 50，扩展为 `localsend-transfer@local`（版本 12），后台为 `1.18.2-local-unified.4`。圆环位于 Vitals 右侧，内部透明，始终显示。22 像素绘制区域中线宽为 2.2 像素，保持原有外径。完整圆环采用闭合路径，无起点接缝；不同颜色在同一圆周连续覆盖。接收色统一为深紫色 `#9A46FF`。

- 白色：后台关闭。单击通过已有 `localsend-cli start --json` 入口启动，顺时针填为绿色。
- 绿色：后台运行。单击直接打开原生文件选择界面，不经过“发送”菜单；选择窗口也可选择文件夹和目标设备。文件和目标设备选择由 Rust＋GTK4 的 `localsend-picker` 实现，通过同一个 Unix 控制套接字提交设备确认和发送请求，不加载 Python 菜单模块，不另开传输后台。Nautilus 右键菜单继续使用 Python。
- 发送：根据后台累计字节，绿色顺时针填成蓝色。单击取消当前发送，不停止后台，也不清空其他排队任务。
- 接收：根据后台累计字节，绿色顺时针填成深紫色。单击取消本次接收。陌生设备仍显示设备名及“接收／拒绝”，四秒超时自动拒绝；确认前或拒绝时圆环不变，接受后进入深紫色接收进度。
- 成功：蓝色或深紫色从后台实际完成时起保留五秒，然后用 700 毫秒逆时针恢复绿色。接收成功后的五秒内，单击深紫色圆环使用系统默认应用打开本批第一个开始接收的文件，采用后台报告的最终保存路径；多文件不重新按文件名或完成顺序排序。发送成功后单击仍无操作。恢复绿色后单击仍为发送入口。
- 失败或取消：保留后台报告的进度，填充部分立即变红；五秒后逆时针恢复绿色，也可以单击提前开始恢复。零字节失败采用红色状态圆环，不改真实字节计数。
- 右键圆环只有“退出”，菜单水平中心对齐圆环中心（靠近屏幕边缘时由 GNOME 避免超出屏幕）。退出时取消仍在进行的任务并关闭后台，当前颜色逆时针退回白色。正常 CLI `stop` 仍保留原先的忙碌保护，显式 `--force` 才取消任务。
- 启动／退出动画至少两秒。实际过程超过两秒时继续等待真实完成信号，并用最多约 180 毫秒收尾；不预测真实启动百分比。强制杀进程只能根据连接消失恢复关闭状态。
- 暂不设计同时收发的独立呈现；仍只有一个圆环，新任务优先显示。系统关闭动画时直接更新颜色；结果五秒期限不变。

扩展只轮询本机 0600 Unix 控制套接字（空闲每秒、活动每 200 毫秒），并监听配置目录下 0600 的 `lifecycle.json` 原子更新。此记录只由持有后台锁的守护进程写入，包含启动、就绪、退出开始和退出完成时间；不改配置、身份或白名单，不增加常驻辅助服务。静态状态无逐帧重绘；原生选择窗口仅打开期间运行，关闭后释放菜单会话，在线确认不自动过期或重复探测。

## 安装与回退

在 `source` 下编译 `cargo build --release --locked --offline --package localsend-cli --package localsend-picker --jobs 2`，再运行 `python3 support/gnome/install_panel.py`。安装器检查后台空闲，备份旧二进制、扩展与菜单，原子替换文件，并保留配置、身份、白名单及其他扩展设置。原本运行的后台会在空闲时重启加载新版本。

GNOME 50 已加载的扩展代码需退出登录后重新登录，锁屏不够；不要强制重启当前 Wayland Shell。重新登录前仍是旧顶栏，新后台兼容旧顶栏接口。不会添加登录自动启动后台。

最近一次安装清单及备份位置为 `development-setup/panel-install.json`。运行 `python3 support/gnome/rollback_panel.py` 恢复该次安装前文件；扩展回退同样需要重新登录。`lifecycle.json` 是可重建状态记录，不包含配置或配对密钥。

## 验证

- `cargo test --locked --offline --package localsend-cli --jobs 2`：含真实回环 TLS 收发、按会话取消、发送进度冻结和队列隔离、启停记录权限及锁互斥。
- `gjs -m support/gnome/test_ring.mjs`：圆环状态、进度边界、真实启停、结果期限、提前恢复及关闭动画。
- `gjs -m support/gnome/test_model.mjs`：单一传输槽的状态选择。
- `check_ring_shell.mjs`：在独立无界面 GNOME 会话中验证 St 绘制、真实指针左右键、菜单、接收确认和禁用清理。启动脚本在 `development-setup/run_ring_shell.sh`，不对当前桌面执行测试注入。

后台输出仍支持原有彩色启动信息，脚本使用 `localsend start --json`、`localsend status --json`。接收目录、文本消息保存规则、TLS、白名单及其他 CLI 功能沿用原有实现。

## Rust 文件和设备选择窗口

`picker/` 是独立 Cargo 成员，使用 gtk-rs（GTK4 ≥ 4.10）。运行时沿用系统 GTK4；构建需 `libgtk-4-dev`。它只在打开选择窗口时运行，关闭时按请求顺序释放 `menu_open` 会话；与 CLI 共用 `$XDG_CONFIG_HOME/localsend-cli/control.sock`，不写配置、身份或白名单。列表仍只显示已配对或已确认在线设备，离线设备禁用，在线确认策略由原后台保持。

只迁移选择窗口时，编译 `cargo build --release --locked --offline -p localsend-picker --jobs 2`，运行 `python3 support/gnome/install_panel.py --picker-only`。此模式只安装 Rust 选择器与顶栏入口，不替换或重启后台，不改 Nautilus 菜单。安装器备份原文件，仍可用原回退脚本恢复。

旧登录会话已经加载的顶栏仍调用 `send_dialog.py`；该文件现在只用 `execv` 将自己替换成 Rust 选择器，当前会话立即可用，不保留 Python 进程。重新登录后，版本 13 的顶栏直接运行 `~/.local/bin/localsend-picker`。不需要为了此次迁移立即注销。

验证：`cargo test --locked --offline -p localsend-picker --jobs 2`；GTK 窗口测试需在独立 Broadway 会话运行同一命令并追加 `-- --ignored --test-threads=1`。原 Python 测试只检查旧入口的兼容启动；保留 Nautilus 的现有测试。

`bash support/gnome/test_picker.sh` 自动建立隔离 Broadway 环境，检查窗口、真实文件／文件夹选择取消、安装回退与 Python 菜单。`bash support/gnome/test_picker_shell.sh` 在独立无界面 GNOME Wayland 会话检查圆环真实点击、新旧启动入口、进程替换及会话释放，并将窗口截图写入 `development-setup/rust-picker-preview/`；不会向日常后台添加测试设备。
