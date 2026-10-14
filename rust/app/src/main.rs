mod remote;
mod settings;
mod terminal;
mod transcript;
mod workspace;

use gpui_kit::*;

fn main() {
    gpui_kit::application().with_assets(gpui_kit::assets::Assets).run(|cx: &mut App| {
        gpui_kit::init(cx);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
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
    });
}
