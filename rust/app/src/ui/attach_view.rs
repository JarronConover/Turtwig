/*! Attachments: files dropped or pasted onto a chat wait as preview chips above its composer while they
upload (to the box, for box chats), then go out with the next message as paths the agent can read. */

use super::style;
use crate::workspace::{stage_one, Attachment, Workspace};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::{Map, Value};
use std::path::PathBuf;

#[derive(Clone, PartialEq)]
pub enum Status {
    Uploading,
    Ready(String),
    Failed(String),
}

#[derive(Clone)]
pub struct Pending {
    pub id: u64,
    pub name: String,
    pub preview: PathBuf,
    pub is_image: bool,
    pub status: Status,
}

pub fn is_image(p: &std::path::Path) -> bool {
    matches!(p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(), Some("png" | "jpg" | "jpeg" | "gif" | "webp"))
}

impl Workspace {
    /** A key of `chat.attachments`: `size`, `image_width_factor`, `image_extensions`, `strip_prefix`, `file_icon`,
    `name_size`, `remove` (size, icon_size, icon, top, right, background, color), `uploading_overlay`, `failed_border`. */
    pub(crate) fn attachment_setting(&self, key: &str) -> Option<Value> {
        self.settings.chat.get("attachments").and_then(|a| a.get(key)).cloned()
    }

    /** Whether file names drop a leading timestamp ("1792…-shot.png" → "shot.png"): `chat.attachments.strip_prefix`. */
    pub(crate) fn strip_stamps(&self) -> bool {
        self.attachment_setting("strip_prefix").and_then(|v| v.as_bool()) != Some(false)
    }

    /** An image by its extension (`chat.attachments.image_extensions`). */
    pub(crate) fn is_image_path(&self, p: &std::path::Path) -> bool {
        match self.attachment_setting("image_extensions").and_then(|v| v.as_array().cloned()) {
            Some(list) => {
                let ext = p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
                list.iter().filter_map(Value::as_str).any(|x| x.eq_ignore_ascii_case(&ext))
            }
            None => is_image(p),
        }
    }

    fn chat_label(&self, chat: &str) -> String {
        if chat == crate::designer::CHAT_ID {
            return self.settings.text("the_designer", "the designer");
        }
        self.chat(chat).map(|c| format!("“{}”", c.name)).unwrap_or_else(|| self.settings.text("this_chat", "this chat"))
    }

    pub(crate) fn attach(&mut self, chat: String, files: Vec<Attachment>, window: &mut Window, cx: &mut Context<Self>) {
        if files.is_empty() {
            return;
        }
        let chat_host = self.chat(&chat).map(|c| c.host.clone());
        let local = chat == crate::designer::CHAT_ID || chat_host.as_deref().is_some_and(|h| self.is_local_host(h));
        let host = (!local).then(|| self.ssh_host_of(&chat_host.unwrap_or_else(|| self.default_host())));
        let what = if files.len() == 1 { self.settings.text("a_file", "a file") } else { self.settings.text("n_files", "{n} files").replace("{n}", &files.len().to_string()) };
        let note = self.settings.text("attaching", "Attaching {what} to {chat}").replace("{what}", &what).replace("{chat}", &self.chat_label(&chat));
        self.toast(note, window, cx);
        for file in files {
            let file = match file {
                Attachment::Bytes { bytes, ext } => match crate::workspace::write_upload(&bytes, &ext) {
                    Ok(p) => Attachment::Path(p),
                    Err(e) => {
                        let t = self.settings.text("attach_failed", "Couldn't attach: {error}").replace("{error}", &e.to_string());
                        self.toast(t, window, cx);
                        continue;
                    }
                },
                other => other,
            };
            let Attachment::Path(path) = file else { continue };
            self.next_attachment += 1;
            let id = self.next_attachment;
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
            let image = self.is_image_path(&path);
            self.attachments.entry(chat.clone()).or_default().push(Pending {
                id,
                name,
                is_image: image,
                preview: path.clone(),
                status: Status::Uploading,
            });
            let (chat, host) = (chat.clone(), host.clone());
            cx.spawn_in(window, async move |this, cx| {
                let result = cx.background_spawn(async move { stage_one(&path, host.as_deref()) }).await;
                this.update_in(cx, |ws, window, cx| {
                    if let Some(p) = ws.attachments.get_mut(&chat).and_then(|v| v.iter_mut().find(|p| p.id == id)) {
                        p.status = match result {
                            Ok(agent_path) => Status::Ready(agent_path),
                            Err(e) => Status::Failed(e),
                        };
                    }
                    let waiting = ws.attachments.get(&chat).is_some_and(|v| v.iter().any(|p| p.status == Status::Uploading));
                    if !waiting && ws.send_when_ready.remove(&chat) {
                        ws.send_composer(&chat, window, cx);
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    /** Paths of the chat's uploaded attachments (clearing them); None while any is still uploading. */
    pub(crate) fn take_attachments(&mut self, chat: &str) -> Option<Vec<String>> {
        let Some(list) = self.attachments.get(chat) else { return Some(vec![]) };
        if list.iter().any(|p| p.status == Status::Uploading) {
            return None;
        }
        let paths = list.iter().filter_map(|p| if let Status::Ready(a) = &p.status { Some(a.clone()) } else { None }).collect();
        self.attachments.remove(chat);
        Some(paths)
    }

    pub(crate) fn attachments_json(&self, chat: &str) -> Value {
        Value::Array(
            self.attachments
                .get(chat)
                .map(|v| {
                    v.iter()
                        .enumerate()
                        .map(|(i, p)| {
                            let (status, error, path) = match &p.status {
                                Status::Uploading => ("uploading", None, None),
                                Status::Ready(a) => ("ready", None, Some(a.clone())),
                                Status::Failed(e) => ("failed", Some(e.clone()), None),
                            };
                            serde_json::json!({"index": i, "name": p.name, "image": p.is_image, "status": status, "error": error, "path": path, "local": p.preview})
                        })
                        .collect()
                })
                .unwrap_or_default(),
        )
    }

    pub(crate) fn attachments_view(&mut self, chat: Option<String>, props: &Map<String, Value>, cx: &mut Context<Self>) -> Option<AnyElement> {
        let chat = chat?;
        let list = self.attachments.get(&chat).filter(|v| !v.is_empty())?.clone();
        let theme = self.settings.theme.clone();
        let cs = self.settings.chat.clone();
        let st = |k: &str| cs.get(k).and_then(Value::as_object).cloned().unwrap_or_default();
        let size = props.get("size").and_then(Value::as_f64).or_else(|| self.attachment_setting("size").and_then(|v| v.as_f64())).unwrap_or(64.) as f32;
        let factor = self.attachment_setting("image_width_factor").and_then(|v| v.as_f64()).unwrap_or(2.5) as f32;
        let color = |k: &str, d: &str| style::color(&self.attachment_setting(k).unwrap_or_else(|| Value::from(d)), &theme);
        let danger = color("failed_border", "@danger").unwrap_or(red());
        let overlay = color("uploading_overlay", "#00000073").unwrap_or(hsla(0., 0., 0., 0.45));
        let rm = self.attachment_setting("remove").unwrap_or(Value::Null);
        let rmf = |k: &str, d: f32| rm.get(k).and_then(Value::as_f64).map(|x| x as f32).unwrap_or(d);
        let rmc = |k: &str, d: &str| style::color(&rm.get(k).cloned().unwrap_or_else(|| Value::from(d)), &theme);
        let (rm_bg, rm_fg) = (rmc("background", "#000000a6").unwrap_or(hsla(0., 0., 0., 0.65)), rmc("color", "#ffffff").unwrap_or(white()));
        let rm_icon = rm.get("icon").and_then(Value::as_str).unwrap_or("x").to_string();
        let file_icon = self.attachment_setting("file_icon").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "file".into());
        let name_size = self.attachment_setting("name_size").and_then(|v| v.as_f64()).unwrap_or(10.) as f32;
        let failed_text = self.settings.text("upload_failed", "Upload failed: {error}");
        let mut row = style::apply(div().flex().flex_row().flex_wrap(), &st("attachments"), &theme);
        for p in list {
            let (chat2, id) = (chat.clone(), p.id);
            let remove = div()
                .id(SharedString::from(format!("att-x:{id}")))
                .absolute()
                .top(px(rmf("top", 3.)))
                .right(px(rmf("right", 3.)))
                .size(px(rmf("size", 18.)))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rm_bg)
                .cursor_pointer()
                .child(Icon::empty().path(format!("icons/{rm_icon}.svg")).size(px(rmf("icon_size", 11.))).text_color(rm_fg))
                .on_click(cx.listener(move |ws, _, _, cx| {
                    if let Some(v) = ws.attachments.get_mut(&chat2) {
                        v.retain(|x| x.id != id);
                    }
                    cx.notify();
                }));
            let failed = if let Status::Failed(e) = &p.status { Some(e.clone()) } else { None };
            let body: AnyElement = if p.is_image {
                img(p.preview.clone()).size_full().object_fit(ObjectFit::Contain).into_any_element()
            } else {
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(4.))
                    .p(px(4.))
                    .child(Icon::empty().path(format!("icons/{file_icon}.svg")).size(px(18.)))
                    .child(div().w_full().text_size(px(name_size)).text_center().truncate().child(p.name.clone()))
                    .into_any_element()
            };
            let width = if p.is_image { crate::workspace::fit(&p.preview, size * factor, size).0.max(size) } else { size };
            let mut chip = style::apply(div().id(SharedString::from(format!("att:{id}"))).relative().w(px(width)).h(px(size)).overflow_hidden(), &st("attachment"), &theme)
                .child(body)
                .when(p.status == Status::Uploading, |d| {
                    d.child(div().absolute().inset_0().flex().items_center().justify_center().bg(overlay).child(Spinner::new().small()))
                })
                .child(remove);
            if let Some(e) = failed {
                let tip = failed_text.replace("{error}", &e);
                chip = chip.border_2().border_color(danger).tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx));
            } else {
                let name = p.name.clone();
                chip = chip.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(name.clone()).build(w, cx));
            }
            row = row.child(chip);
        }
        Some(row.into_any_element())
    }
}
