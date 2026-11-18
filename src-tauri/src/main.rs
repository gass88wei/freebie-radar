use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, RunEvent};
use tauri_plugin_shell::process::CommandChild;
use tauri_plugin_shell::ShellExt;

struct SidecarState(Mutex<Option<CommandChild>>);

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
    unsafe {
        libc::signal(libc::SIGTERM, kill_sidecar_on_signal as libc::sighandler_t);
        libc::signal(libc::SIGINT, kill_sidecar_on_signal as libc::sighandler_t);
        libc::signal(libc::SIGHUP, kill_sidecar_on_signal as libc::sighandler_t);
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

            // 外部环境变量优先，便于 CI 冒烟测试与排障；未设置时用桌面版的默认值。
            let port = std::env::var("PORT").unwrap_or_else(|_| "8788".to_string());
            let db_path = std::env::var("DATABASE_PATH").unwrap_or_else(|_| {
                data_dir.join("radar.db").to_string_lossy().to_string()
            });

            // spawn sidecar
            let resource_dir = app.path().resource_dir().expect("resource dir");
            // dist/server.mjs is bundled by esbuild; placed at <res>/src/server/server.mjs so the
            // server's `../../dist/web` (relative to import.meta.url) resolves to <res>/dist/web.
            let server_entry = resource_dir
                .join("src")
                .join("server")
                .join("server.mjs");
            let (_rx, child) = app
                .shell()
                .sidecar("node")
                .expect("sidecar")
                .args([server_entry.to_string_lossy().to_string()])
                .current_dir(data_dir.clone())
                .env("DATABASE_PATH", db_path)
                .env("PORT", &port)
                .env("HOST", "127.0.0.1")
                .env("TZ", "Asia/Shanghai")
                .env(
                    "ENABLE_SCHEDULER",
                    std::env::var("ENABLE_SCHEDULER").unwrap_or_else(|_| "true".to_string()),
                )
                .spawn()
                .expect("spawn sidecar");

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
                eprintln!("sidecar did not become healthy");
            }

            // 窗口已在 tauri.conf.json 里声明（label = main），这里只把它导航到实际端口再显示，
            // 避免重复创建同名窗口导致 panic。
            let window = app
                .get_webview_window("main")
                .expect("main window declared in tauri.conf.json");
            let app_url = format!("http://127.0.0.1:{}", port);
            if let Ok(url) = app_url.parse::<tauri::Url>() {
                let _ = window.navigate(url);
            }
            let _ = window.show();

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
