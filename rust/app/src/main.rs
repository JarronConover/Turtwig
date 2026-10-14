mod designer;
mod diff;
#[cfg(target_os = "macos")]
#[allow(deprecated)]
mod macos;
mod remote;
mod settings;
mod terminal;
mod transcript;
mod ui;
mod workspace;

use gpui_kit::*;

fn open_main(cx: &mut App) {
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("Agent Box".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(20.), px(15.))),
        }),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1500.), px(940.)), cx))),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| workspace::Workspace::new(window, cx)))
        .expect("open the main window");
    cx.activate(true);
}

fn main() {
    let app = gpui_kit::application().with_assets(gpui_kit::assets::AllAssets);
    // Clicking the Dock icon (or opening the app again) after closing the window brings it back.
    app.on_reopen(|cx| {
        if cx.windows().is_empty() {
            open_main(cx);
        }
    });
    app.run(|cx: &mut App| {
        gpui_kit::init(cx);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
        cx.on_action(|_: &workspace::Quit, cx| cx.quit());
        cx.set_menus([Menu {
            name: "Agent Box".into(),
            items: vec![
                MenuItem::action("Settings…", workspace::OpenSettings),
                MenuItem::action("Command Palette", workspace::Palette),
                MenuItem::separator(),
                MenuItem::action("Quit Agent Box", workspace::Quit),
            ],
            disabled: false,
        }]);
        open_main(cx);
    });
}
