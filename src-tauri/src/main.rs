use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, RunEvent};
use tauri_plugin_shell::process::CommandChild;
use tauri_plugin_shell::ShellExt;

struct SidecarState(Mutex<Option<CommandChild>>);

/// 极简时间戳，免得为了写日志引入 chrono 依赖。
fn chrono_now() -> String {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs().to_string(),
        Err(_) => "0".to_string(),
    }
}

/// sidecar 的 PID：信号处理器不是 async-signal-safe，只能存原始 pid，退出时用它兜底杀进程。
static SIDECAR_PID: OnceLock<i32> = OnceLock::new();

/// 外层进程被 kill / 注销 / 关机时，RunEvent::Exit 不一定触发，这里补一条信号兜底，
/// 否则 node 侧车会变成孤儿进程继续占用端口和内存。
#[cfg(unix)]
extern "C" fn kill_sidecar_on_signal(_sig: i32) {
    if let Some(pid) = SIDECAR_PID.get() {
        unsafe {
            libc::kill(*pid, libc::SIGKILL);
        }
    }
    unsafe {
        libc::_exit(1);
    }
}

#[cfg(unix)]
fn install_signal_handlers() {
    // clippy::function_casts_as_integer：函数项要先转成指针再转整数
    let handler = kill_sidecar_on_signal as *const () as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGHUP, handler);
    }
}

#[cfg(not(unix))]
fn install_signal_handlers() {}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
fn main() {
    install_signal_handlers();
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![]),
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(SidecarState(Mutex::new(None)))
        .setup(|app| {
            let data_dir = app.path().app_data_dir().expect("app data dir");
            std::fs::create_dir_all(&data_dir).ok();
            let log_path = data_dir.join("desktop.log");
            let log = move |msg: String| {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path)
                {
                    let _ = writeln!(f, "[{}] {}", chrono_now(), msg);
                }
            };
            log(format!("启动，data_dir={:?}", data_dir));

            // PORT 允许为空字符串时不要当成有效值：空串会让窗口连到 80 端口。
            let port = std::env::var("PORT")
                .ok()
                .and_then(|v| v.trim().parse::<u16>().ok())
                .map(|p| p.to_string())
                .unwrap_or_else(|| "8788".to_string());
            let db_path = std::env::var("DATABASE_PATH")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| data_dir.join("radar.db").to_string_lossy().to_string());
            log(format!("port={} db={}", port, db_path));

            // spawn sidecar
            let resource_dir = app.path().resource_dir().expect("resource dir");
            // 资源目录兜底：APPDIR/APPIMAGE 等环境变量（例如从 AppImage 包内启动的父进程）
            // 会污染 Tauri 的推断，导致资源目录指向别处。这里从可执行文件路径反推一份候选。
            let mut candidates = vec![resource_dir.clone()];
            if let Ok(exe) = std::env::current_exe() {
                if let Some(dir) = exe.parent() {
                    candidates.push(dir.to_path_buf());
                    // deb 安装布局：/usr/lib/<app>/<app>，资源就在同目录
                    if let Some(parent) = dir.parent() {
                        candidates.push(parent.to_path_buf());
                    }
                }
            }
            let server_entry = candidates
                .iter()
                .map(|base| base.join("src").join("server").join("server.mjs"))
                .find(|p| p.exists());
            let Some(server_entry) = server_entry else {
                let msg = format!(
                    "找不到 server.mjs，已尝试：{:?}",
                    candidates
                        .iter()
                        .map(|b| b.join("src/server/server.mjs"))
                        .collect::<Vec<_>>()
                );
                log(msg.clone());
                eprintln!("[radar-desktop] {msg}");
                return Ok(());
            };
            log(format!("server_entry={:?}", server_entry));

            let sidecar_cmd = app.shell().sidecar("node").expect("sidecar");
            let (mut rx, child) = match sidecar_cmd
                .args([server_entry.to_string_lossy().to_string()])
                .current_dir(data_dir.clone())
                .env("DATABASE_PATH", &db_path)
                .env("PORT", &port)
                .env("HOST", "127.0.0.1")
                .env("TZ", "Asia/Shanghai")
                .env(
                    "ENABLE_SCHEDULER",
                    std::env::var("ENABLE_SCHEDULER").unwrap_or_else(|_| "true".to_string()),
                )
                .spawn()
            {
                Ok(v) => {
                    log(format!("sidecar 已启动 pid={}", v.1.pid()));
                    v
                }
                Err(e) => {
                    log(format!("sidecar 启动失败: {e}"));
                    return Ok(());
                }
            };

            // 必须持续消费 sidecar 的 stdout/stderr：
            // 丢弃接收端会让管道缓冲填满，node 写日志时被阻塞，服务随之卡死。
            let srv_log_path = data_dir.join("server.log");
            std::thread::spawn(move || {
                use std::io::Write;
                while let Some(event) = rx.blocking_recv() {
                    if let tauri_plugin_shell::process::CommandEvent::Stdout(bytes) = &event {
                        if let Ok(mut f) = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&srv_log_path)
                        {
                            let _ = f.write_all(bytes);
                        }
                    } else if let tauri_plugin_shell::process::CommandEvent::Stderr(bytes) = &event {
                        if let Ok(mut f) = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&srv_log_path)
                        {
                            let _ = f.write_all(bytes);
                        }
                    }
                }
            });

            {
                let state = app.state::<SidecarState>();
                let _ = SIDECAR_PID.set(child.pid() as i32);
                *state.0.lock().unwrap() = Some(child);
            }

            // poll /health up to 30s
            let health_url = format!("http://127.0.0.1:{}/health", port);
            let mut ready = false;
            for _ in 0..60 {
                if reqwest::blocking::get(&health_url)
                    .map(|r| r.status().is_success())
                    .unwrap_or(false)
                {
                    ready = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            if !ready {
                log("sidecar 30 秒内未就绪".to_string());
            } else {
                log("sidecar health OK".to_string());
            }

            // 直接用外部 URL 建窗口：前端所有请求都是相对路径（/api/...），
            // 必须让页面 origin 就是 http://127.0.0.1:<port>。
            // 若把窗口声明在 tauri.conf.json 里，窗口最初加载的是内置 tauri://localhost，
            // 再 navigate 过去也容易停在旧 origin，表现为页面能渲染但 /api 全部 Load failed。
            let app_url = format!("http://127.0.0.1:{}", port);
            let url = app_url.parse::<tauri::Url>().expect("valid app url");
            let window = tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::External(url),
            )
            .title("资源雷达")
            .inner_size(1280.0, 800.0)
            .center()
            .build();
            let window = match window {
                Ok(w) => w,
                Err(e) => {
                    log(format!("创建窗口失败: {e}"));
                    return Ok(());
                }
            };
            let _ = window.show();
            if !ready {
                // 别让用户只看到一个浏览器默认错误页，把排查线索直接写进窗口。
                let hint = format!(
                    "<html><body style=\"font-family:sans-serif;padding:24px;line-height:1.6\">\
                     <h3>后端服务未能启动</h3>\
                     <p>已尝试连接 <code>http://127.0.0.1:{port}</code>，但没有响应。</p>\
                     <p>详细日志：<code>{}/desktop.log</code></p>\
                     <p>常见原因：端口 {port} 被其它程序占用；或安装包缺少 node 侧车。</p></body></html>",
                    data_dir.display()
                );
                let _ = window.eval(&format!(
                    "document.open();document.write({});document.close();",
                    serde_json::to_string(&hint).unwrap_or_else(|_| "\"\"".to_string())
                ));
            }

            // tray menu
            let show_i = MenuItem::with_id(app, "show", "显示", true, None::<&str>).unwrap();
            let hide_i = MenuItem::with_id(app, "hide", "隐藏", true, None::<&str>).unwrap();
            let browser_i =
                MenuItem::with_id(app, "browser", "在浏览器打开", true, None::<&str>).unwrap();
            let quit_i = MenuItem::with_id(app, "quit", "退出", true, None::<&str>).unwrap();
            let menu = Menu::with_items(app, &[&show_i, &hide_i, &browser_i, &quit_i]).unwrap();
            let _tray = TrayIconBuilder::with_id("main-tray")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "hide" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.hide();
                        }
                    }
                    "browser" => {
                        #[allow(deprecated)]
                        let _ = tauri_plugin_shell::ShellExt::shell(app)
                            .open("http://127.0.0.1:8788", None);
                    }
                    "quit" => {
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)
                .unwrap();

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            if let RunEvent::Exit = event {
                let state = app_handle.state::<SidecarState>();
                let child = state.0.lock().unwrap().take();
                if let Some(child) = child {
                    let _ = child.kill();
                }
            }
        });
}
