// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::{env, fs::File, ops::Deref, path::Path, sync::RwLock};

use config::Config;
use fs::{fs_exist_dir, fs_make_dir, read_json_file, write_json_file};
use locale::Locale;
use tray::Tray;

mod config;
mod fs;
mod locale;
mod tray;
mod utils;

use crate::utils::download_with_progress;
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_autostart::ManagerExt;
// use crate::utils::ensure_single_instance;
#[cfg(target_os = "windows")]
use crate::utils::find_first_available_drive_letter;
#[cfg(target_os = "windows")]
use crate::utils::is_winfsp_installed;
//#[cfg(target_os = "windows")]
//use crate::utils::set_window_shadow;

pub(crate) type Runtime = tauri::Wry;

pub trait State: Send + Sync + 'static {}
pub struct StateWrapper<T: State>(RwLock<T>);

pub trait AppExt {
    fn app_main_window(&self) -> tauri::WebviewWindow<Runtime>;
    fn with_app_state<T: State, R>(&self, closure: impl FnOnce(&T) -> R) -> R;
    fn set_app_state<T: State>(&self, state: T);
    fn update_app_config(&self) -> anyhow::Result<()>;
    fn write_app_config(&self, config: Config) -> anyhow::Result<()>;
    fn app_data_dir(&self) -> PathBuf;
    fn app_config_file(&self) -> PathBuf;
    fn app_quit(&self);
    fn app_restart(&self);
}

impl<M: tauri::Manager<Runtime>> AppExt for M {
    fn app_main_window(&self) -> tauri::WebviewWindow {
        self.get_webview_window("main").unwrap()
    }

    fn with_app_state<T: State, R>(&self, closure: impl FnOnce(&T) -> R) -> R {
        let wrapper = self.state::<StateWrapper<T>>();
        let state = wrapper.deref().0.read().unwrap();
        closure(state.deref())
    }

    fn set_app_state<T: State>(&self, state: T) {
        if let Some(wrapper) = self.try_state::<StateWrapper<T>>() {
            *wrapper.deref().0.write().unwrap() = state;
        } else {
            self.manage(StateWrapper(RwLock::new(state)));
        }
    }

    fn update_app_config(&self) -> anyhow::Result<()> {
        self.with_app_state::<Config, _>(|config| {
            let current_locale = tauri_plugin_os::locale().unwrap_or_else(|| "C".into());
            self.set_app_state(Locale::new(
                config.0["settings"]
                    .get("language")
                    .map(|item| item.as_str().unwrap())
                    .unwrap_or_else(|| &current_locale),
            ));
            self.set_app_state(Tray::new(self.app_handle())?);
            Ok(())
        })
    }

    fn write_app_config(&self, config: Config) -> anyhow::Result<()> {
        self.set_app_state(config);
        let file = File::create(self.app_config_file())?;
        serde_json::to_writer_pretty(
            file,
            &self.with_app_state::<Config, _>(|config| config.0.clone()),
        )?;
        Ok(())
    }

    fn app_data_dir(&self) -> PathBuf {
        self.path().home_dir().unwrap().join(".netmount")
    }

    fn app_config_file(&self) -> PathBuf {
        self.app_data_dir().join("config.json")
    }

    fn app_quit(&self) {
        self.app_handle().exit(0)
    }

    fn app_restart(&self) {
        self.app_handle().restart()
    }
}

pub trait WindowExt {
    fn toggle_devtools(&self, preferred_open: Option<bool>);
    fn toggle_visibility(&self, preferred_show: Option<bool>) -> anyhow::Result<()>;
}

impl WindowExt for tauri::WebviewWindow<Runtime> {
    fn toggle_devtools(&self, preferred_open: Option<bool>) {
        let open = preferred_open.unwrap_or_else(|| !self.is_devtools_open());
        if open {
            self.open_devtools()
        } else {
            self.close_devtools()
        }
    }

    fn toggle_visibility(&self, preferred_show: Option<bool>) -> anyhow::Result<()> {
        let open = preferred_show.unwrap_or_else(|| !self.is_visible().unwrap_or(false));
        if open {
            self.show()?;
            self.set_focus()?;
        } else {
            self.hide()?;
        }
        Ok(())
    }
}

pub fn init() -> anyhow::Result<()> {
    // 设置运行目录
    let exe_dir = env::current_exe()
        .expect("无法获取当前可执行文件路径")
        .parent()
        .expect("无法获取父目录")
        .to_path_buf();
    println!("exe_dir: {}", exe_dir.display());

    let binding = env::current_exe().expect("Failed to get the current executable path");
    let exe_flie_name = Path::new(&binding)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap();

    if !cfg!(debug_assertions) {
        if cfg!(target_os = "linux") {
            let resources_dir = exe_dir
                .parent()
                .expect("无法获取父目录")
                .join("lib")
                .join(exe_flie_name);
            env::set_current_dir(&resources_dir).expect("更改工作目录失败");
        }

        if cfg!(target_os = "windows") {
            env::set_current_dir(&exe_dir).expect("更改工作目录失败");
        }

        if cfg!(target_os = "macos") {
            // 在macOS上，进一步定位到.app内部的Contents/Resources目录
            let resources_dir = exe_dir.parent().expect("无法获取父目录").join("Resources");
            println!("resources_dir: {}", resources_dir.display());
            // 设置运行目录到Resources
            if let Err(e) = env::set_current_dir(&resources_dir) {
                eprintln!("更改工作目录到Resources失败: {}", e);
                // 根据实际情况处理错误，如返回错误信息或终止程序
            }
        }
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            app.app_main_window().toggle_visibility(Some(true)).ok();
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![]),
        ))
        .invoke_handler(tauri::generate_handler![
            toggle_devtools,
            get_config,
            update_config,
            get_language_pack,
            download_file,
            get_autostart_state,
            set_autostart_state,
            get_winfsp_install_state,
            get_available_drive_letter,
            get_available_ports,
            get_temp_dir,
            fs_exist_dir,
            fs_make_dir,
            restart_self,
            read_json_file,
            write_json_file
        ])
        .setup(|app| {
            //判断配置目录是否存在，如不存在创建配置目录
            let config_dir = app.app_data_dir();
            if !config_dir.exists() {
                std::fs::create_dir_all(&config_dir).expect("创建配置目录失败");
                println!("创建配置目录成功");
            }

            //配置文件
            if let Some(file) = File::open(app.app_config_file()).ok() {
                app.set_app_state(Config(serde_json::from_reader(file)?))
            } else {
                app.write_app_config(Config::default())?
            };
            app.update_app_config()?;

            //开发者工具
            #[cfg(debug_assertions)]
            app.app_main_window().toggle_devtools(Some(true));
            Ok(())
        })
        .run(tauri::generate_context!())?;
    Ok(())
}

#[tauri::command]
fn toggle_devtools(window: tauri::WebviewWindow<Runtime>, preferred_open: Option<bool>) {
    window.toggle_devtools(preferred_open)
}

#[tauri::command]
fn get_language_pack(app: tauri::AppHandle<Runtime>) -> serde_json::Value {
    serde_json::Value::Object(serde_json::value::Map::from_iter(
        app.with_app_state::<Locale, _>(|locale| locale.0)
            .entries()
            .map(|(&key, &value)| (key.into(), serde_json::Value::String(value.into()))),
    ))
    .into()
}

#[tauri::command]
fn get_config(app: tauri::AppHandle<Runtime>) -> serde_json::Value {
    app.with_app_state::<Config, _>(|config| config.0.clone())
}

#[tauri::command]
fn update_config(
    app: tauri::AppHandle<Runtime>,
    data: serde_json::Value,
) -> anyhow_tauri::TAResult<()> {
    app.write_app_config(Config(data))?;
    app.update_app_config()?;
    Ok(())
}

#[tauri::command]
fn restart_self(app: tauri::AppHandle<Runtime>) {
    app.restart()
}

#[tauri::command]
fn get_winfsp_install_state() -> Result<bool, usize> {
    #[cfg(not(target_os = "windows"))]
    return Ok(false);

    #[cfg(target_os = "windows")]
    match is_winfsp_installed() {
        Ok(is_enabled) => Ok(is_enabled),
        Err(_) => Ok(false),
    }
}

#[tauri::command]
fn get_autostart_state(app: tauri::AppHandle<Runtime>) -> Result<bool, usize> {
    let autostart_manager = app.autolaunch();
    match autostart_manager.is_enabled() {
        Ok(is_enabled) => Ok(is_enabled),
        Err(_) => Ok(false),
    }
}

#[tauri::command]
fn set_autostart_state(app: tauri::AppHandle<Runtime>, enabled: bool) -> Result<bool, ()> {
    let autostart_manager = app.autolaunch();
    Ok(if enabled {
        autostart_manager.enable().is_ok()
    } else {
        autostart_manager.disable().is_ok()
    })
}

#[tauri::command]
fn download_file(url: String, out_path: String) -> Result<bool, usize> {
    download_with_progress(&url, &out_path, |total_size, downloaded| {
        println!(
            "下载进度: {}/{}  {}%",
            total_size,
            downloaded,
            (100 * downloaded / total_size)
        );
    })
    .expect("下载失败");
    Ok(true)
}

#[tauri::command]
fn get_available_drive_letter() -> Result<String, String> {
    #[cfg(not(target_os = "windows"))]
    return Ok(String::from(""));
    #[cfg(target_os = "windows")]
    match find_first_available_drive_letter() {
        Ok(Some(drive)) => Ok(drive),
        Ok(None) => Ok(String::from("")),
        Err(e) => Ok(format!("{}", e)),
    }
}

#[tauri::command]
fn get_available_ports(count: usize) -> Vec<u16> {
    return utils::get_available_ports(count);
}

#[tauri::command]
fn get_temp_dir() -> String {
    std::env::temp_dir().to_str().unwrap().to_owned()
}
