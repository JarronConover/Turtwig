mod completions;
mod designer;
mod diff;
mod jsc;
mod logo;
#[cfg(target_os = "macos")]
#[allow(deprecated)]
mod macos;
mod remote;
mod script;
mod settings;
mod terminal;
mod transcript;
mod ui;
mod workspace;

use gpui_kit::*;

fn open_main(cx: &mut App) {
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("a.box".into()),
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

/** The built-in icon set, plus `icons/NAME.svg` from the settings folder (which wins) and a few brand marks it lacks. */
struct Assets;

const GITLAB_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor"><path d="m23.6 9.6-.03-.09-3.27-8.52a.85.85 0 0 0-1.62.09l-2.2 6.75H7.53L5.32 1.08a.85.85 0 0 0-1.62-.09L.43 9.51l-.03.09a6.07 6.07 0 0 0 2.01 7.01l.01.01.03.02 4.98 3.73 2.46 1.86 1.5 1.13a1 1 0 0 0 1.22 0l1.5-1.13 2.46-1.86 5.01-3.75.01-.01a6.07 6.07 0 0 0 2.01-7.01z"/></svg>"#;

impl gpui_kit::AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        if let Some(name) = path.strip_prefix("icons/") {
            if let Ok(bytes) = std::fs::read(settings::dir().join("icons").join(name)) {
                return Ok(Some(bytes.into()));
            }
            if name == "gitlab.svg" {
                return Ok(Some(GITLAB_SVG.into()));
            }
        }
        gpui_kit::AssetSource::load(&gpui_kit::assets::AllAssets, path)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        gpui_kit::AssetSource::list(&gpui_kit::assets::AllAssets, path)
    }
}

fn main() {
    // `--iconset DIR` writes the bundle icon's PNGs from the settings' logo (bundle.sh turns them into AppIcon.icns).
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--iconset") {
        if let Err(e) = logo::write_iconset(std::path::Path::new(args.get(2).map(String::as_str).unwrap_or("AppIcon.iconset"))) {
            eprintln!("{e:#}");
            std::process::exit(1);
        }
        return;
    }
    let app = gpui_kit::application().with_assets(Assets);
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
            name: "a.box".into(),
            items: vec![
                MenuItem::action("Settings…", workspace::OpenSettings),
                MenuItem::action("Command Palette", workspace::Palette),
                MenuItem::separator(),
                MenuItem::action("Quit a.box", workspace::Quit),
            ],
            disabled: false,
        }]);
        open_main(cx);
    });
}
