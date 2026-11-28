//! Non-text message content: user attachments (image thumbnails, file chips, removed marker),
//! tool-result images, documents and audio, and cited sources. Remote images are never loaded
//! (design §8): they show as a host-named link opened by the core's URL check.

use std::sync::Arc;

use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Div, Image, ImageFormat, InteractiveElement as _,
    IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, px,
};
use spanbay_desktop::core::agent::attachments::sniff;
use spanbay_desktop::core::agent::model::{AgentAttachment, AgentSourceDto, AttachmentKind};
use spanbay_desktop::core::app::{
    agent_attachment_get_bytes_impl, agent_attachment_open_impl, agent_image_open_impl,
    system_open_external_impl,
};
use spanbay_desktop::core::model::hub::{HubCitation, HubImage, HubPart};
use spanbay_desktop::state::{decode_inline_image, format_human_bytes};

use crate::bridge::spawn_blocking_into;
use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, btn_icon, btn_secondary, choice_chip, h_row, icon_xs,
    image_actual, image_fit, image_thumb, kv_chip, mono_font_family, palette, to_hsla,
    viewport_modal,
};

use super::ChatView;
use super::types::{ChatModal, ImageViewerSource};

/// Maximum raw byte size of an image decoded into the UI texture cache (20 MiB).
const MAX_DECODE_IMAGE_BYTES: usize = 20 * 1024 * 1024;
/// Maximum pixel count (`width * height`) of an image decoded into the UI texture cache.
const MAX_IMAGE_PIXELS: u64 = 64_000_000;
/// Maximum single-edge dimension in pixels for an image decoded into the UI texture cache.
const MAX_IMAGE_EDGE_PX: u32 = 8192;
/// Maximum entries retained in `ChatView::media_cache`.
const MAX_MEDIA_CACHE_ENTRIES: usize = 128;

/// Type alias for cached decoded image textures in `ChatView::media_cache`.
pub type CachedMedia = Arc<Image>;

fn image_from(bytes: Vec<u8>, media_type_hint: &str) -> Option<Arc<Image>> {
    if bytes.is_empty() || bytes.len() > MAX_DECODE_IMAGE_BYTES {
        return None;
    }
    let info = sniff(&bytes, "", media_type_hint)?;
    if info.kind != AttachmentKind::Image {
        return None;
    }
    let (w, h) = info.width.zip(info.height)?;
    if w == 0
        || h == 0
        || w > MAX_IMAGE_EDGE_PX
        || h > MAX_IMAGE_EDGE_PX
        || u64::from(w) * u64::from(h) > MAX_IMAGE_PIXELS
    {
        return None;
    }
    let fmt = ImageFormat::from_mime_type(info.media_type)?;
    Some(Arc::new(Image::from_bytes(fmt, bytes)))
}

/// Decodes a `data:<type>;base64,<data>` URL (attachment thumbnails).
fn image_from_data_url(url: &str) -> Option<Arc<Image>> {
    let (media_type, data) = url.strip_prefix("data:")?.split_once(";base64,")?;
    let (bytes, mime) = decode_inline_image(&HubImage::Base64 {
        media_type: media_type.to_owned(),
        data: data.to_owned(),
    })?;
    image_from(bytes, &mime)
}

/// The host of an `http(s)` URL, for link labels.
fn url_host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

impl ChatView {
    /// A cached image; the first call decodes it (`decode`) or, for a stored attachment without
    /// inline bytes, fetches it in the background and re-renders when it arrives.
    fn cached_image(
        &self,
        key: &str,
        decode: impl FnOnce() -> Option<Arc<Image>>,
        fetch: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) -> Option<Arc<Image>> {
        if let Some(hit) = self.media_cache.borrow().get(key) {
            return hit.clone();
        }
        let decoded = decode();
        if decoded.is_none()
            && let Some((attachment_id, media_type)) = fetch
        {
            let key_done = key.to_owned();
            spawn_blocking_into(
                cx,
                move |state| agent_attachment_get_bytes_impl(state, &attachment_id),
                move |this, res, cx| {
                    let image = res.ok().and_then(|bytes| image_from(bytes, &media_type));
                    let mut cache = this.media_cache.borrow_mut();
                    if cache.len() >= MAX_MEDIA_CACHE_ENTRIES && !cache.contains_key(&key_done) {
                        cache.clear();
                    }
                    cache.insert(key_done, image);
                    drop(cache);
                    cx.notify();
                },
            )
            .detach();
        }
        let mut cache = self.media_cache.borrow_mut();
        if cache.len() >= MAX_MEDIA_CACHE_ENTRIES && !cache.contains_key(key) {
            cache.clear();
        }
        cache.insert(key.to_owned(), decoded.clone());
        decoded
    }

    fn open_external_url(&mut self, url: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |_| system_open_external_impl(&url),
            |this, res, cx| {
                if let Err(e) = res {
                    this.status_msg = Some((Tone::Danger, e.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    /// Toggles Fit / Actual size in an open image viewer modal.
    pub fn set_image_viewer_actual(&mut self, actual: bool, cx: &mut Context<Self>) {
        if let Some(ChatModal::ImageViewer { actual: cur, .. }) = &mut self.active_modal {
            *cur = actual;
            cx.notify();
        }
    }

    /// Hands the image in the viewer off to the OS default app (temp copy written by the core).
    pub fn open_viewer_image_in_system(
        &mut self,
        source: ImageViewerSource,
        cx: &mut Context<Self>,
    ) {
        let task = spawn_blocking_into(
            cx,
            move |state| match source {
                ImageViewerSource::Attachment(id) => agent_attachment_open_impl(state, &id),
                ImageViewerSource::Inline { bytes, media_type } => {
                    agent_image_open_impl(state, &bytes, &media_type)
                }
            },
            |this, res, cx| {
                if let Err(e) = res {
                    this.status_msg = Some((Tone::Danger, e.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }
}

fn link_chip(
    id: SharedString,
    label: String,
    url: String,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let p = palette(cx);
    h_row()
        .id(id)
        .gap(px(4.0))
        .px(px(7.0))
        .py(px(2.0))
        .rounded(px(6.0))
        .bg(to_hsla(p.sunken))
        .text_xs()
        .text_color(to_hsla(p.accent_ink))
        .cursor_pointer()
        .child(icon_xs(UiIcon::ExternalLink))
        .child(SharedString::from(label))
        .on_click(cx.listener(move |this, _, _, cx| this.open_external_url(url.clone(), cx)))
}

/// Formats `1440×900 · 179 KB` from whatever dimension and size info is known.
fn format_image_meta(dims: Option<(u32, u32)>, size_bytes: Option<u64>) -> String {
    let mut parts = Vec::with_capacity(2);
    if let Some((w, h)) = dims {
        parts.push(format!("{w}×{h}"));
    }
    if let Some(sz) = size_bytes {
        parts.push(format_human_bytes(sz));
    }
    parts.join(" · ")
}

/// A thumbnail that opens the image viewer.
fn viewer_thumb(
    key: String,
    title: String,
    meta: String,
    dims: Option<(u32, u32)>,
    source: Option<ImageViewerSource>,
    image: Arc<Image>,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    div()
        .id(SharedString::from(format!("thumb-{key}")))
        .cursor_pointer()
        .child(image_thumb(image, cx))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.active_modal = Some(ChatModal::ImageViewer {
                key: key.clone(),
                title: title.clone(),
                meta: meta.clone(),
                dims,
                actual: false,
                source: source.clone(),
            });
            cx.notify();
        }))
        .into_any_element()
}

/// In-app lightbox viewer for an attachment or inline MCP image (Fit / Actual size / Open in system app).
#[allow(clippy::too_many_arguments)]
pub(super) fn render_image_viewer_modal(
    view: &ChatView,
    key: &str,
    title: String,
    meta: String,
    dims: Option<(u32, u32)>,
    actual: bool,
    source: Option<ImageViewerSource>,
    window: &Window,
    cx: &mut Context<ChatView>,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let image = match &source {
        Some(ImageViewerSource::Attachment(att_id)) => view
            .cached_image(key, || None, Some((att_id.clone(), String::new())), cx)
            .or_else(|| {
                view.media_cache
                    .borrow()
                    .get(&format!("att-thumb-{att_id}"))
                    .cloned()
                    .flatten()
            }),
        _ => view.media_cache.borrow().get(key).cloned().flatten(),
    };
    let on_close_x = on_close.clone();
    let header = h_row()
        .w_full()
        .flex_shrink_0()
        .justify_between()
        .gap(px(10.0))
        .px(px(14.0))
        .py(px(10.0))
        .bg(to_hsla(p.overlay))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.overlay)))
        .child(
            h_row()
                .flex_1()
                .min_w_0()
                .gap(px(8.0))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(SharedString::from(title)),
                )
                .children((!meta.is_empty()).then(|| {
                    div()
                        .flex_shrink_0()
                        .font_family(mono_font_family(cx))
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(SharedString::from(meta))
                })),
        )
        .child(
            h_row()
                .flex_shrink_0()
                .gap(px(6.0))
                .child(
                    choice_chip("chat-img-fit", ch.viewer_fit(), !actual, cx).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.set_image_viewer_actual(false, cx);
                        }),
                    ),
                )
                .child(
                    choice_chip("chat-img-actual", ch.viewer_actual(), actual, cx).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.set_image_viewer_actual(true, cx);
                        }),
                    ),
                )
                .children(source.map(|src| {
                    btn_secondary("chat-img-open-sys", ch.viewer_open_system()).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.open_viewer_image_in_system(src.clone(), cx);
                        }),
                    )
                }))
                .child(
                    btn_icon("chat-img-close", UiIcon::Close)
                        .tooltip(crate::ui::tr(cx).common().close())
                        .on_click(on_close_x),
                ),
        )
        .into_any_element();
    let body = if actual {
        div()
            .id("chat-img-actual-scroll")
            .size_full()
            .bg(to_hsla(p.sunken))
            .overflow_scrollbar()
            .p(px(16.0))
            .children(image.map(|img| image_actual(img, dims)))
            .into_any_element()
    } else {
        div()
            .size_full()
            .bg(to_hsla(p.sunken))
            .p(px(12.0))
            .children(image.map(image_fit))
            .into_any_element()
    };
    viewport_modal(
        "chat-image-modal",
        0.86,
        0.84,
        header,
        body,
        div().into_any_element(),
        window,
        cx,
        on_close,
    )
}

/// Attachments of a user message, right-aligned under the bubble.
pub fn render_user_attachments(
    view: &ChatView,
    attachments: &[AgentAttachment],
    cx: &mut Context<ChatView>,
) -> Option<Div> {
    if attachments.is_empty() {
        return None;
    }
    let ch = crate::ui::tr(cx).chat();
    let mut row = h_row().gap(px(6.0)).flex_wrap().justify_end();
    for att in attachments {
        if att.removed {
            row = row.child(kv_chip(att.name.clone(), ch.attachment_removed(), cx));
            continue;
        }
        let full_key = format!("att-{}", att.id);
        let image = (att.kind == AttachmentKind::Image)
            .then(|| {
                if let Some(thumb) = att.thumb_data_url.clone() {
                    let thumb_key = format!("att-thumb-{}", att.id);
                    view.cached_image(
                        &thumb_key,
                        move || image_from_data_url(&thumb),
                        Some((att.id.clone(), att.media_type.clone())),
                        cx,
                    )
                } else {
                    view.cached_image(
                        &full_key,
                        || None,
                        Some((att.id.clone(), att.media_type.clone())),
                        cx,
                    )
                }
            })
            .flatten();
        row = match image {
            Some(img) => {
                let dims = att.width.zip(att.height);
                let meta = format_image_meta(dims, Some(att.size_bytes));
                row.child(viewer_thumb(
                    full_key,
                    att.name.clone(),
                    meta,
                    dims,
                    Some(ImageViewerSource::Attachment(att.id.clone())),
                    img,
                    cx,
                ))
            }
            None => row.child(kv_chip(
                att.name.clone(),
                format_human_bytes(att.size_bytes),
                cx,
            )),
        };
    }
    Some(row)
}

/// Images, documents and audio of a tool result (an MCP tool may return them).
pub fn render_result_media(
    view: &ChatView,
    call_id: &str,
    parts: &[HubPart],
    cx: &mut Context<ChatView>,
) -> Option<Div> {
    let ch = crate::ui::tr(cx).chat();
    let mut row = h_row().gap(px(6.0)).flex_wrap();
    let mut any = false;
    for (ix, part) in parts.iter().enumerate() {
        let el = match part {
            HubPart::Image { image } => {
                let key = format!("res-{call_id}-{ix}");
                match image {
                    HubImage::Url { url } => {
                        any = true;
                        row = row.child(link_chip(
                            SharedString::from(format!("res-url-{call_id}-{ix}")),
                            ch.media_remote_image(url_host(url)),
                            url.clone(),
                            cx,
                        ));
                        continue;
                    }
                    HubImage::Base64 { .. } => {
                        let decoded = view.cached_image(
                            &key,
                            || {
                                let (bytes, mime) = decode_inline_image(image)?;
                                image_from(bytes, &mime)
                            },
                            None,
                            cx,
                        );
                        match decoded {
                            Some(img) => {
                                let inline_img = image.clone();
                                let title = ch.media_image().to_owned();
                                div()
                                    .id(SharedString::from(format!("thumb-{key}")))
                                    .cursor_pointer()
                                    .child(image_thumb(img, cx))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let (dims, size_bytes, source) =
                                            if let Some((bytes, mime)) =
                                                decode_inline_image(&inline_img)
                                            {
                                                let d = sniff(&bytes, "", &mime)
                                                    .and_then(|s| s.width.zip(s.height));
                                                let sz = Some(bytes.len() as u64);
                                                let src = Some(ImageViewerSource::Inline {
                                                    bytes: Arc::<[u8]>::from(bytes),
                                                    media_type: mime,
                                                });
                                                (d, sz, src)
                                            } else {
                                                (None, None, None)
                                            };
                                        this.active_modal = Some(ChatModal::ImageViewer {
                                            key: key.clone(),
                                            title: title.clone(),
                                            meta: format_image_meta(dims, size_bytes),
                                            dims,
                                            actual: false,
                                            source,
                                        });
                                        cx.notify();
                                    }))
                                    .into_any_element()
                            }
                            None => kv_chip(ch.media_image(), String::new(), cx).into_any_element(),
                        }
                    }
                    HubImage::AttachmentRef {
                        attachment_id,
                        media_type,
                    } => {
                        let decoded = view.cached_image(
                            &key,
                            || None,
                            Some((attachment_id.clone(), media_type.clone())),
                            cx,
                        );
                        match decoded {
                            Some(img) => viewer_thumb(
                                key,
                                ch.media_image().to_owned(),
                                String::new(),
                                None,
                                Some(ImageViewerSource::Attachment(attachment_id.clone())),
                                img,
                                cx,
                            ),
                            None => kv_chip(ch.media_image(), String::new(), cx).into_any_element(),
                        }
                    }
                }
            }
            HubPart::Document { document } => kv_chip(
                ch.media_document(),
                document
                    .name
                    .clone()
                    .unwrap_or_else(|| document.media_type.clone()),
                cx,
            )
            .into_any_element(),
            HubPart::Audio { media_type, .. } => {
                kv_chip(ch.media_audio(media_type), String::new(), cx).into_any_element()
            }
            _ => continue,
        };
        any = true;
        row = row.child(el);
    }
    any.then_some(row)
}

/// Sources a model cited while streaming (`CitationAdded`) or in a text part.
pub fn render_citations(scope: &str, citations: &[HubCitation], cx: &mut Context<ChatView>) -> Div {
    let mut row = source_row(cx);
    for (ix, c) in citations.iter().enumerate() {
        row = row.child(link_chip(
            SharedString::from(format!("cite-{scope}-{ix}-{}", c.url)),
            format!("{} · {}", c.title, url_host(&c.url)),
            c.url.clone(),
            cx,
        ));
    }
    row
}

/// Sources stored on an assistant message (`meta.sources`).
pub fn render_sources(msg_id: &str, sources: &[AgentSourceDto], cx: &mut Context<ChatView>) -> Div {
    let mut row = source_row(cx);
    for s in sources {
        row = row.child(link_chip(
            SharedString::from(format!("src-{msg_id}-{}", s.source_id)),
            format!("{} · {}", s.title, url_host(&s.url)),
            s.url.clone(),
            cx,
        ));
    }
    row
}

fn source_row(cx: &mut Context<ChatView>) -> Div {
    let p = palette(cx);
    h_row().gap(px(6.0)).flex_wrap().child(
        div()
            .text_xs()
            .text_color(to_hsla(p.muted))
            .child(crate::ui::tr(cx).chat().sources_label()),
    )
}

#[cfg(test)]
mod tests {
    use super::url_host;

    #[test]
    fn host_of_urls() {
        assert_eq!(url_host("https://docs.rs/gpui?x=1"), "docs.rs");
        assert_eq!(url_host("http://a.b:8080/x"), "a.b:8080");
        assert_eq!(url_host("example.com"), "example.com");
    }
}
