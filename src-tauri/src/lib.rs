use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Listener, Manager, WebviewUrl, WebviewWindowBuilder,
};

#[cfg(target_os = "macos")]
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
#[cfg(target_os = "macos")]
use tauri::window::{Color, Effect, EffectState, EffectsBuilder};
#[cfg(target_os = "macos")]
use tauri::RunEvent;

// Injected into every page load. Detects light/dark theme from <html> and <body>,
// then reports it to Rust via Tauri's IPC bridge.
const THEME_DETECT_SCRIPT: &str = r#"
(function(){
    function getTheme(){
        var el;
        // Check <html>
        el=document.documentElement;
        if(el){
            if(el.classList.contains('dark'))return'dark';
            var a=el.getAttribute('data-theme');
            if(a==='dark')return'dark';
            a=el.getAttribute('data-color-scheme');
            if(a==='dark')return'dark';
            a=el.getAttribute('data-mode');
            if(a==='dark')return'dark';
            if(el.hasAttribute('dark'))return'dark';
        }
        // Check <body> (DeepSeek puts "light"/"dark" in body class)
        el=document.body;
        if(el){
            for(var i=0;i<el.classList.length;i++){
                var c=el.classList[i];
                if(c==='dark')return'dark';
                if(c==='light')return'light';
            }
        }
        return'light';
    }
    function report(t){
        // Try both IPC channels — invoke() is preferred, emit() is a fallback
        try{window.__TAURI_INTERNALS__.invoke('report_theme',{theme:t}).catch(function(){})}catch(e){}
        try{window.__TAURI_INTERNALS__.emit('theme-changed',{theme:t})}catch(e){}
    }
    // Observe both html and body for attribute/class changes
    function setup(){
        report(getTheme());
        var cb=function(){report(getTheme())},opts={attributes:true,attributeFilter:['class','data-theme','data-color-scheme','data-mode','style'],subtree:false};
        [document.documentElement,document.body].filter(Boolean).forEach(function(n){
            new MutationObserver(cb).observe(n,opts);
        });
    }
    if(document.readyState==='loading')document.addEventListener('DOMContentLoaded',setup);
    else setup();
    // Backup polling: every 500ms for 10s, covering delayed theme application
    var n=0,i=setInterval(function(){report(getTheme());if(++n>=20)clearInterval(i)},500);
})();
"#;

/// Tauri command: called from JS via invoke() to report theme changes.
/// This is the primary mechanism; the event listener is kept as a fallback.
#[tauri::command]
fn report_theme(window: tauri::WebviewWindow, theme: String) {
    // Must dispatch to main thread for NSAppearance/NSApp API
    let w = window.clone();
    let _ = window.run_on_main_thread(move || {
        apply_window_theme(&w, &theme);
    });
}

/// Update the macOS window chrome to match the web page's theme.
#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
fn apply_window_theme(window: &tauri::WebviewWindow, theme: &str) {
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSApp, NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua};

        let mtm = MainThreadMarker::new().expect("must be on main thread");

        match theme {
            "dark" => {
                let _ = window.set_effects(
                    EffectsBuilder::new()
                        .effect(Effect::Sidebar)
                        .state(EffectState::Active)
                        .radius(0.0)
                        .color(Color(0, 0, 0, 255))
                        .build(),
                );
                let appearance = unsafe { NSAppearance::appearanceNamed(NSAppearanceNameDarkAqua) };
                if let Some(ref a) = appearance {
                    NSApp(mtm).setAppearance(Some(a));
                }
            }
            _ => {
                let _ = window.set_effects(
                    EffectsBuilder::new()
                        .effect(Effect::ContentBackground)
                        .state(EffectState::Active)
                        .radius(0.0)
                        .color(Color(255, 255, 255, 255))
                        .build(),
                );
                let appearance = unsafe { NSAppearance::appearanceNamed(NSAppearanceNameAqua) };
                if let Some(ref a) = appearance {
                    NSApp(mtm).setAppearance(Some(a));
                }
            }
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Track webview zoom level as percentage (100 = 100%)
    let zoom_level = Arc::new(AtomicU32::new(100));

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .invoke_handler(tauri::generate_handler![report_theme])
        .on_menu_event(move |app, event| {
            if let Some(window) = app.get_webview_window("main") {
                match event.id().as_ref() {
                    "zoom_in" => {
                        let new = (zoom_level.load(Ordering::Relaxed) + 10).min(500);
                        zoom_level.store(new, Ordering::Relaxed);
                        let _ = window.set_zoom(new as f64 / 100.0);
                    }
                    "zoom_out" => {
                        let new = (zoom_level.load(Ordering::Relaxed).saturating_sub(10)).max(25);
                        zoom_level.store(new, Ordering::Relaxed);
                        let _ = window.set_zoom(new as f64 / 100.0);
                    }
                    "zoom_reset" => {
                        zoom_level.store(100, Ordering::Relaxed);
                        let _ = window.set_zoom(1.0);
                    }
                    "quit" => app.exit(0),
                    _ => {}
                }
            }
        })
        .setup(|app| {
            // Create the main window, loading DeepSeek directly + theme detection script
            let window = WebviewWindowBuilder::new(
                app,
                "main",
                WebviewUrl::External("https://chat.deepseek.com".parse().expect("invalid URL")),
            )
            .title("DeepSeek")
            .inner_size(1200.0, 800.0)
            .min_inner_size(400.0, 300.0);
            #[cfg(target_os = "macos")]
            let window = window
                .hidden_title(true)
                .title_bar_style(tauri::TitleBarStyle::Transparent);
            let window = window
            .resizable(true)
            .zoom_hotkeys_enabled(true)
            .initialization_script(THEME_DETECT_SCRIPT)
            .build()?;

            // Default to dark; the web page's theme-changed event will correct it
            apply_window_theme(&window, "dark");

            // Listen for theme changes emitted by the web page
            let w = window.clone();
            window.listen("theme-changed", move |event| {
                let payload: serde_json::Value =
                    serde_json::from_str(event.payload()).unwrap_or_default();
                let theme = payload
                    .get("theme")
                    .and_then(|v| v.as_str())
                    .unwrap_or("light")
                    .to_string();
                let w2 = w.clone();
                let _ = w.run_on_main_thread(move || {
                    apply_window_theme(&w2, &theme);
                });
            });

            // --- App menu bar (macOS) ---
            #[cfg(target_os = "macos")]
            {
                let zoom_in =
                    MenuItem::with_id(app, "zoom_in", "Zoom In", true, Some("CmdOrCtrl+="))?;
                let zoom_out =
                    MenuItem::with_id(app, "zoom_out", "Zoom Out", true, Some("CmdOrCtrl+-"))?;
                let zoom_reset =
                    MenuItem::with_id(app, "zoom_reset", "Actual Size", true, Some("CmdOrCtrl+0"))?;

                let edit_menu = Submenu::with_items(
                    app,
                    "Edit",
                    true,
                    &[
                        &PredefinedMenuItem::undo(app, None::<&str>)?,
                        &PredefinedMenuItem::redo(app, None::<&str>)?,
                        &PredefinedMenuItem::separator(app)?,
                        &PredefinedMenuItem::cut(app, None::<&str>)?,
                        &PredefinedMenuItem::copy(app, None::<&str>)?,
                        &PredefinedMenuItem::paste(app, None::<&str>)?,
                        &PredefinedMenuItem::separator(app)?,
                        &PredefinedMenuItem::select_all(app, None::<&str>)?,
                    ],
                )?;

                let view_menu = Submenu::with_items(
                    app,
                    "View",
                    true,
                    &[
                        &zoom_in,
                        &zoom_out,
                        &PredefinedMenuItem::separator(app)?,
                        &zoom_reset,
                    ],
                )?;

                let app_menu = Submenu::with_items(
                    app,
                    "DeepSeek",
                    true,
                    &[
                        &PredefinedMenuItem::about(app, Some("About DeepSeek"), None)?,
                        &PredefinedMenuItem::separator(app)?,
                        &PredefinedMenuItem::services(app, None::<&str>)?,
                        &PredefinedMenuItem::separator(app)?,
                        &PredefinedMenuItem::hide(app, None::<&str>)?,
                        &PredefinedMenuItem::hide_others(app, None::<&str>)?,
                        &PredefinedMenuItem::show_all(app, None::<&str>)?,
                        &PredefinedMenuItem::separator(app)?,
                        &PredefinedMenuItem::quit(app, None::<&str>)?,
                    ],
                )?;

                let window_menu = Submenu::with_items(
                    app,
                    "Window",
                    true,
                    &[
                        &PredefinedMenuItem::minimize(app, None::<&str>)?,
                        &PredefinedMenuItem::fullscreen(app, None::<&str>)?,
                        &PredefinedMenuItem::separator(app)?,
                        &PredefinedMenuItem::close_window(app, None::<&str>)?,
                        &PredefinedMenuItem::bring_all_to_front(app, None::<&str>)?,
                    ],
                )?;

                app.set_menu(Menu::with_items(
                    app,
                    &[&app_menu, &edit_menu, &view_menu, &window_menu],
                )?)?;
            }

            // --- System tray ---
            let show = MenuItemBuilder::with_id("show", "Show DeepSeek").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
            let menu = MenuBuilder::new(app).items(&[&show, &quit]).build()?;

            let img = image::load_from_memory(include_bytes!("../icons/icon.png"))
                .expect("failed to load tray icon")
                .to_rgba8();
            let (width, height) = img.dimensions();
            let icon = tauri::image::Image::new_owned(img.into_raw(), width, height);

            TrayIconBuilder::new()
                .icon(icon)
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                })
                .build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, _event| {
            #[cfg(target_os = "macos")]
            if let RunEvent::Reopen { .. } = _event {
                if let Some(w) = _app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
        });
}
