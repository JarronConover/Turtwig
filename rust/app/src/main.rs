use gpui::*;

struct Hello;

impl Render for Hello {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(rgb(0x212122)).text_color(rgb(0xf5f5f7)).child("Agent Box")
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| Hello)).unwrap();
    });
}
