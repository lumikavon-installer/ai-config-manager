#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod agent_server;
use ai_config_core::rest as rest_api;
mod settings;
mod sync_scheduler;
use ai_config_core::sync::{
    default_git_branch, ApplicationRegistration, CreateRepositoryRequest, CreateRepositoryResult,
    NativeSyncSecrets, RepositoryTestRequest, RepositoryTestResult, SyncPolicy, SyncProvider,
    SyncRecord, SyncStore, SyncView,
};
use ai_config_core::{
    ConfigView, EndpointKind, Error, HistoryEntry, NativeVault, ProbeResult, SaveRequest, Store,
};
use serde::Serialize;
use settings::{Settings, SettingsState};
use std::sync::{Arc, Mutex};
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Emitter, Manager, State,
};
use tauri_plugin_autostart::ManagerExt;

type Core = Arc<Store>;
type Sync = Arc<SyncStore>;
type Preferences = Arc<Mutex<SettingsState>>;
fn error(code: &str, message: &str) -> Error {
    Error {
        code: code.into(),
        message: message.into(),
    }
}
async fn blocking<T, F>(work: F) -> Result<T, Error>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, Error> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|_| error("WORKER_FAILED", "Background operation failed"))?
}
#[tauri::command]
async fn config_read(store: State<'_, Core>) -> Result<ConfigView, Error> {
    let store = store.inner().clone();
    blocking(move || store.load()).await
}
#[tauri::command]
async fn config_save(store: State<'_, Core>, request: SaveRequest) -> Result<ConfigView, Error> {
    let store = store.inner().clone();
    blocking(move || store.save(request)).await
}
#[tauri::command]
async fn config_check(store: State<'_, Core>) -> Result<(), Error> {
    let store = store.inner().clone();
    blocking(move || store.validate_secret()).await
}
#[tauri::command]
async fn config_test(store: State<'_, Core>) -> Result<ProbeResult, Error> {
    let store = store.inner().clone();
    blocking(move || store.test_endpoint()).await
}
#[tauri::command]
async fn endpoint_read(store: State<'_, Core>, kind: EndpointKind) -> Result<ConfigView, Error> {
    let store = store.inner().clone();
    blocking(move || store.load_endpoint(kind)).await
}
#[tauri::command]
async fn endpoint_save(
    store: State<'_, Core>,
    kind: EndpointKind,
    request: SaveRequest,
) -> Result<ConfigView, Error> {
    let store = store.inner().clone();
    blocking(move || store.save_endpoint(kind, request, "desktop")).await
}
#[tauri::command]
async fn endpoint_check(store: State<'_, Core>, kind: EndpointKind) -> Result<(), Error> {
    let store = store.inner().clone();
    blocking(move || store.validate_endpoint(kind)).await
}
#[tauri::command]
async fn endpoint_test(store: State<'_, Core>, kind: EndpointKind) -> Result<ProbeResult, Error> {
    let store = store.inner().clone();
    blocking(move || store.test_endpoint_kind(kind)).await
}
#[tauri::command]
async fn history_list(store: State<'_, Core>) -> Result<Vec<HistoryEntry>, Error> {
    let store = store.inner().clone();
    blocking(move || store.history()).await
}
#[tauri::command]
async fn sync_read(sync: State<'_, Sync>) -> Result<SyncView, Error> {
    let sync = sync.inner().clone();
    blocking(move || Ok(sync.view())).await
}
#[tauri::command]
async fn sync_save_settings(
    sync: State<'_, Sync>,
    repo: String,
    github_token: Option<String>,
    encryption_secret: Option<String>,
    provider: Option<SyncProvider>,
    git_branch: Option<String>,
) -> Result<SyncView, Error> {
    let sync = sync.inner().clone();
    blocking(move || {
        sync.save_repository_settings(
            provider.unwrap_or_default(),
            &repo,
            &git_branch.unwrap_or_else(default_git_branch),
            github_token,
            encryption_secret,
        )
    })
    .await
}
#[tauri::command]
async fn sync_test_repository(
    sync: State<'_, Sync>,
    request: RepositoryTestRequest,
) -> Result<RepositoryTestResult, Error> {
    let sync = sync.inner().clone();
    blocking(move || sync.test_repository(request)).await
}
#[tauri::command]
async fn sync_create_repository(
    sync: State<'_, Sync>,
    request: CreateRepositoryRequest,
) -> Result<CreateRepositoryResult, Error> {
    let sync = sync.inner().clone();
    blocking(move || sync.create_github_repository(request)).await
}
#[tauri::command]
async fn sync_add_item(
    sync: State<'_, Sync>,
    label: Option<String>,
    path: String,
    enabled: bool,
) -> Result<SyncView, Error> {
    let sync = sync.inner().clone();
    blocking(move || sync.add_item(label, &path, enabled)).await
}
#[tauri::command]
async fn sync_remove_item(sync: State<'_, Sync>, id: String) -> Result<SyncView, Error> {
    let sync = sync.inner().clone();
    blocking(move || sync.remove_item(&id)).await
}
#[tauri::command]
async fn sync_set_item_enabled(
    sync: State<'_, Sync>,
    id: String,
    enabled: bool,
) -> Result<SyncView, Error> {
    let sync = sync.inner().clone();
    blocking(move || sync.set_item_enabled(&id, enabled)).await
}
#[tauri::command]
async fn sync_run(app: tauri::AppHandle, sync: State<'_, Sync>) -> Result<SyncView, Error> {
    let sync = sync.inner().clone();
    blocking(move || {
        let result = sync.run_sync();
        let _ = app.emit("sync-changed", ());
        let _ = app.emit("config-changed", ());
        result
    })
    .await
}
#[tauri::command]
async fn sync_run_application(
    app: tauri::AppHandle,
    sync: State<'_, Sync>,
    app_id: String,
) -> Result<SyncView, Error> {
    let sync = sync.inner().clone();
    blocking(move || {
        let result = sync.run_application(&app_id, ai_config_core::sync::SyncTrigger::Manual);
        let _ = app.emit("sync-changed", ());
        result
    })
    .await
}
#[tauri::command]
async fn sync_set_application_auto_sync(
    app: tauri::AppHandle,
    sync: State<'_, Sync>,
    app_id: String,
    enabled: bool,
) -> Result<SyncView, Error> {
    let sync = sync.inner().clone();
    blocking(move || {
        let result = sync.set_application_auto_sync(&app_id, enabled);
        let _ = app.emit("sync-changed", ());
        result
    })
    .await
}
#[tauri::command]
async fn sync_register_application(
    sync: State<'_, Sync>,
    request: ApplicationRegistration,
) -> Result<SyncView, Error> {
    let sync = sync.inner().clone();
    blocking(move || sync.register_application(request)).await
}
#[tauri::command]
async fn sync_save_policy(sync: State<'_, Sync>, policy: SyncPolicy) -> Result<SyncView, Error> {
    let sync = sync.inner().clone();
    blocking(move || sync.save_policy(policy)).await
}
#[tauri::command]
async fn sync_history(sync: State<'_, Sync>) -> Result<Vec<SyncRecord>, Error> {
    let sync = sync.inner().clone();
    blocking(move || sync.history()).await
}
#[tauri::command]
async fn sync_cleanup(sync: State<'_, Sync>) -> Result<SyncView, Error> {
    let sync = sync.inner().clone();
    blocking(move || sync.cleanup()).await
}
fn app_name(language: &str) -> &'static str {
    if language == "en" {
        "AI Config Manager"
    } else {
        "AI配置管理器"
    }
}
fn update_visible_language(app: &tauri::AppHandle, language: &str) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_title(app_name(language));
    }
    if let Some(tray) = app.tray_by_id("main-tray") {
        let _ = tray.set_tooltip(Some(app_name(language)));
    }
}
#[derive(Serialize)]
struct SettingsView {
    #[serde(flatten)]
    settings: Settings,
    autostart: bool,
    autostart_known: bool,
    version: String,
    platform: String,
    tray_available: bool,
    warnings: Vec<String>,
    sync: Option<SyncView>,
}
fn settings_view(app: &tauri::AppHandle) -> Result<SettingsView, Error> {
    let state = app.state::<Preferences>();
    let state = state
        .lock()
        .map_err(|_| error("SETTINGS_LOCK", "Preferences are busy"))?;
    let mut warnings = state.warnings.clone();
    let (autostart, autostart_known) = match app.autolaunch().is_enabled() {
        Ok(value) => (value, true),
        Err(_) => {
            warnings.push("AUTOSTART_UNKNOWN: Cannot read OS login registration; displayed status is not confirmed.".into());
            (false, false)
        }
    };
    let sync = app
        .try_state::<Arc<SyncStore>>()
        .map(|sync| sync.inner().view());
    Ok(SettingsView {
        settings: state.settings.clone(),
        autostart,
        autostart_known,
        version: app.package_info().version.to_string(),
        platform: std::env::consts::OS.into(),
        tray_available: state.tray_available,
        warnings,
        sync,
    })
}
#[tauri::command]
async fn settings_read(app: tauri::AppHandle) -> Result<SettingsView, Error> {
    blocking(move || settings_view(&app)).await
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsUpdate {
    silent_start: bool,
    close_to_tray: bool,
    language: String,
}
#[tauri::command]
async fn settings_save(
    app: tauri::AppHandle,
    settings: SettingsUpdate,
) -> Result<SettingsView, Error> {
    let other = app.clone();
    blocking(move || {
        let preferences = other.state::<Preferences>();
        let mut state = preferences
            .lock()
            .map_err(|_| error("SETTINGS_LOCK", "Preferences are busy"))?;
        let mut next = state.settings.clone();
        next.silent_start = settings.silent_start;
        next.close_to_tray = settings.close_to_tray;
        next.language = settings.language;
        state.save(next).map_err(|m| error("SETTINGS_SAVE", &m))
    })
    .await?;
    // Native menu mutation is marshalled by Tauri to the UI thread.
    let language = app
        .state::<Preferences>()
        .lock()
        .map_err(|_| error("SETTINGS_LOCK", "Preferences are busy"))?
        .settings
        .language
        .clone();
    update_visible_language(&app, &language);
    if let Some(tray) = app.tray_by_id("main-tray") {
        let refreshed =
            tray_menu(&app, &language).and_then(|menu| tray.set_menu(Some(menu)).map(|_| ()));
        if refreshed.is_err() {
            // The preferences are already committed. Record why the visible tray
            // still shows the previous language instead of only returning an error
            // that disappears with the next navigation.
            if let Ok(mut state) = app.state::<Preferences>().lock() {
                if !state
                    .warnings
                    .iter()
                    .any(|w| w.starts_with("TRAY_MENU_STALE"))
                {
                    state.warnings.push("TRAY_MENU_STALE: Preferences were saved, but the tray menu could not be rebuilt. Tray labels may still show the previous language until the app restarts.".into());
                }
            }
            return Err(error(
                "TRAY_MENU",
                "Preferences saved, but tray menu could not be refreshed",
            ));
        }
    }
    blocking(move || settings_view(&app)).await
}
#[tauri::command]
async fn autostart_set(app: tauri::AppHandle, enabled: bool) -> Result<bool, Error> {
    let query_app = app.clone();
    let actual = blocking(move || {
        let manager = query_app.autolaunch();
        if enabled {
            manager.enable()
        } else {
            manager.disable()
        }
        .map_err(|_| error("AUTOSTART_UPDATE", "Could not update OS login registration"))?;
        let actual = manager
            .is_enabled()
            .map_err(|_| error("AUTOSTART_QUERY", "Could not verify OS login registration"))?;
        if actual != enabled {
            return Err(error(
                "AUTOSTART_MISMATCH",
                "OS login registration did not match the requested setting",
            ));
        }
        Ok(actual)
    })
    .await?;
    // An explicit user choice is authoritative even if the one-time install
    // default had not been marked yet.
    let preferences_state = app.state::<Preferences>();
    if let Ok(mut preferences) = preferences_state.lock() {
        if !preferences.settings.autostart_default_applied {
            preferences.settings.autostart_default_applied = true;
            let chosen = preferences.settings.clone();
            if preferences.save(chosen).is_err() {
                preferences.warnings.push("AUTOSTART_DEFAULT_SAVE: The explicit launch-at-login choice is active, but the preference could not be saved.".into());
            }
        }
    }
    Ok(actual)
}
#[tauri::command]
fn window_hide(app: tauri::AppHandle) -> Result<(), Error> {
    let available = app
        .state::<Preferences>()
        .lock()
        .map_err(|_| error("SETTINGS_LOCK", "Preferences are busy"))?
        .tray_available;
    if !available {
        return Err(error(
            "TRAY_UNAVAILABLE",
            "Cannot hide this window because the system tray is unavailable",
        ));
    }
    app.get_webview_window("main")
        .ok_or_else(|| error("WINDOW_MISSING", "Main window is missing"))?
        .hide()
        .map_err(|_| error("WINDOW_HIDE", "Could not hide window"))
}
fn show(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}
#[derive(Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum WindowAction {
    Minimize,
    ToggleMaximize,
    Close,
    StartDragging,
    IsMaximized,
}
#[tauri::command]
fn window_control(window: tauri::WebviewWindow, action: WindowAction) -> Result<bool, Error> {
    let result = match action {
        WindowAction::Minimize => window.minimize(),
        WindowAction::ToggleMaximize => window.is_maximized().and_then(|maximized| {
            if maximized {
                window.unmaximize()
            } else {
                window.maximize()
            }
        }),
        // Use the normal close request so close-to-tray remains authoritative.
        WindowAction::Close => window.close(),
        WindowAction::StartDragging => window.start_dragging(),
        WindowAction::IsMaximized => Ok(()),
    };
    result.map_err(|_| error("WINDOW_CONTROL", "Could not update the window"))?;
    if matches!(
        action,
        WindowAction::Close | WindowAction::Minimize | WindowAction::StartDragging
    ) {
        return Ok(false);
    }
    window
        .is_maximized()
        .map_err(|_| error("WINDOW_CONTROL", "Could not read the window state"))
}
fn tray_menu(app: &tauri::AppHandle, lang: &str) -> tauri::Result<Menu<tauri::Wry>> {
    let labels = if lang == "en" {
        ["Show", "Hide", "Settings", "Quit"]
    } else {
        ["显示窗口", "隐藏窗口", "设置", "退出"]
    };
    let show = MenuItem::with_id(app, "show", labels[0], true, None::<&str>)?;
    let hide = MenuItem::with_id(app, "hide", labels[1], true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", labels[2], true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", labels[3], true, None::<&str>)?;
    Menu::with_items(app, &[&show, &hide, &settings, &quit])
}
// Linux may create a tray handle even when the desktop has no tray host.
// A conservative host check prevents an unreachable silent application.
fn tray_host_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        let Ok(connection) = zbus::blocking::Connection::session() else {
            return false;
        };
        let Ok(proxy) = zbus::blocking::Proxy::new(
            &connection,
            "org.kde.StatusNotifierWatcher",
            "/StatusNotifierWatcher",
            "org.kde.StatusNotifierWatcher",
        ) else {
            return false;
        };
        proxy
            .get_property::<bool>("IsStatusNotifierHostRegistered")
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}
fn main() {
    let app = tauri::Builder::default()
        // Register first: a second launch exits and restores the existing window,
        // including when the first instance is minimized or hidden in the tray.
        .plugin(tauri_plugin_single_instance::init(|app, _, _| show(app)))
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec!["--silent"])))
        .setup(|app| {
            let store = Arc::new(Store::new(Store::default_path()?, Arc::new(NativeVault::new())));
            app.manage(store.clone());
            let app_config = app.path().app_config_dir()?;
            let sync = Arc::new(SyncStore::new(
                app_config.join("sync.json"),
                store.clone(),
                Arc::new(NativeSyncSecrets::new()),
            ));
            app.manage(sync.clone());
            app.manage(sync_scheduler::SyncScheduler::start(app.handle().clone(), sync.clone()));
            let path = app_config.join("settings.json");
            let mut preferences = SettingsState::load(path);
            if preferences.settings.autostart_default_applied {
                // Refresh the registration path after an update or rename, but
                // never override a user's later decision to disable autostart.
                if app.autolaunch().is_enabled().unwrap_or(false) {
                    let _ = app.autolaunch().enable();
                }
            } else {
                let enabled = app
                    .autolaunch()
                    .enable()
                    .and_then(|_| app.autolaunch().is_enabled());
                match enabled {
                    Ok(true) => {
                        preferences.settings.autostart_default_applied = true;
                        let chosen = preferences.settings.clone();
                        if preferences.save(chosen).is_err() {
                            preferences.warnings.push("AUTOSTART_DEFAULT_SAVE: Launch-at-login was enabled, but the default preference could not be saved; it will be checked again on the next launch.".into());
                        }
                    }
                    _ => preferences.warnings.push("AUTOSTART_DEFAULT_FAILED: Launch-at-login could not be enabled by default. Enable it from Settings after checking system permissions.".into()),
                }
            }
            update_visible_language(app.handle(), &preferences.settings.language);
            let menu = tray_menu(app.handle(), &preferences.settings.language);
            let tray = menu.and_then(|menu| {
                TrayIconBuilder::with_id("main-tray")
                    .icon(tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png"))?)
                    .tooltip(app_name(&preferences.settings.language)).menu(&menu).show_menu_on_left_click(true)
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "show" => show(app), "hide" => { let _ = window_hide(app.clone()); },
                        "settings" => { show(app); let _ = app.emit("navigate", "settings"); },
                        "quit" => app.exit(0), _ => {}
                    }).build(app)
            });
            preferences.tray_available = tray.is_ok() && tray_host_available();
            if !preferences.tray_available { preferences.warnings.push("TRAY_UNAVAILABLE: System tray creation or host detection failed; silent start and hide-to-tray are disabled for this session.".into()); }
            let agent = match agent_server::AgentServer::start(app.handle().clone(), store, sync) {
                Ok(server) => Some(server),
                Err(_) => {
                    preferences.warnings.push("AGENT_UNAVAILABLE: The local agent socket could not be started; the CLI and Skill will use the configuration file directly.".into());
                    None
                }
            };
            let silent = std::env::args().any(|a| a == "--silent") || preferences.settings.silent_start;
            let visible = !silent || !preferences.tray_available || !preferences.warnings.is_empty();
            app.manage(Arc::new(Mutex::new(preferences)));
            if let Some(agent) = agent { app.manage(agent); }
            if visible { show(app.handle()); }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.app_handle().state::<Preferences>();
                if let Ok(state) = state.lock() {
                    if state.settings.close_to_tray && state.tray_available {
                        if window.hide().is_ok() { api.prevent_close(); }
                    }
                };
            }
        })
        .invoke_handler(tauri::generate_handler![
            config_read,
            config_save,
            config_check,
            config_test,
            endpoint_read,
            endpoint_save,
            endpoint_check,
            endpoint_test,
            history_list,
            settings_read,
            settings_save,
            sync_read,
            sync_save_settings,
            sync_add_item,
            sync_remove_item,
            sync_set_item_enabled,
            sync_run,
            sync_run_application,
            sync_set_application_auto_sync,
            sync_test_repository,
            sync_create_repository,
            sync_register_application,
            sync_save_policy,
            sync_history,
            sync_cleanup,
            autostart_set,
            window_hide,
            window_control
        ])
        .build(tauri::generate_context!())
        .expect("AI配置管理器 could not start");
    app.run(|app, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            if let Some(scheduler) = app.try_state::<sync_scheduler::SyncScheduler>() {
                scheduler.stop();
            }
            if let Some(server) = app.try_state::<agent_server::AgentServer>() {
                server.stop();
            }
        }
    });
}
