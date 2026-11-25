//! Phase 0 / Console 4-point native GPU widget benchmark panel:
//! 1. Single-line `InputState`, multiline `TextareaState`, and Tree-sitter JSON `EditorState` (CJK IME)
//! 2. Selectable streaming Markdown (`TextView::markdown().selectable(true).stream_fade(true)`)
//! 3. 10,000-row virtualized `DataTable`
//! 4. 10 MiB JSON Rope `EditorState` with load timing

use std::time::Instant;

use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div, px,
};
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::i18n::tr as tr_lang;

use crate::ui::{
    Column, EditorState, InputState, TableDelegate, TableState, TextareaState, Tone,
    active_language, badge, btn_primary, btn_secondary, code_editor, h_row, multiline_input,
    palette, section_heading, streaming_markdown_view, table_column, text_input, to_hsla, tr,
    v_col, virtual_table,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchTab {
    Ime,
    StreamingMarkdown,
    VirtualTable10k,
    LargeJson10Mb,
}

/// Virtual table delegate backing 10,000 live benchmark rows at 120fps.
pub struct BenchTableDelegate {
    columns: Vec<Column>,
    rows: Vec<(usize, String, String, String, String)>,
}

impl BenchTableDelegate {
    #[must_use]
    pub fn new(row_count: usize) -> Self {
        let methods = ["GET", "POST", "PUT", "PATCH", "DELETE"];
        let models = [
            "claude-sonnet-4-6",
            "gpt-5.2",
            "gemini-2.5-pro",
            "deepseek-r1",
            "qwen3-coder",
        ];
        let rows = (0..row_count)
            .map(|i| {
                (
                    i + 1,
                    methods[i % methods.len()].to_owned(),
                    models[i % models.len()].to_owned(),
                    format!("{} ms", 18 + (i * 37) % 840),
                    format!("${:.5}", ((i % 400) as f64) * 0.00003),
                )
            })
            .collect();
        Self {
            columns: vec![
                table_column("idx", "#").width(px(64.0)),
                table_column("method", "Method").width(px(86.0)),
                table_column("model", "Model / Route").width(px(240.0)),
                table_column("latency", "Latency").width(px(110.0)),
                table_column("cost", "Cost").width(px(110.0)),
            ],
            rows,
        }
    }
}

impl TableDelegate for BenchTableDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        self.columns
            .get(col_ix)
            .cloned()
            .unwrap_or_else(|| table_column("?", "?"))
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let p = palette(cx);
        let text = self
            .rows
            .get(row_ix)
            .map(|r| match col_ix {
                0 => r.0.to_string(),
                1 => r.1.clone(),
                2 => r.2.clone(),
                3 => r.3.clone(),
                4 => r.4.clone(),
                _ => String::new(),
            })
            .unwrap_or_default();
        div()
            .px(px(8.0))
            .text_xs()
            .text_color(to_hsla(p.ink))
            .child(SharedString::from(text))
    }
}

/// Stateful holder for the 4 native widget benchmark workloads.
pub struct NativeBenchState {
    pub tab: BenchTab,
    lang: Language,
    pub ime_input: Entity<InputState>,
    pub ime_textarea: Entity<TextareaState>,
    pub ime_editor: Entity<EditorState>,
    pub large_json_editor: Entity<EditorState>,
    pub large_json_stats: String,
    pub last_json_load_ms: Option<f64>,
    pub stream_md: String,
    pub stream_tokens: usize,
    pub table_state: Entity<TableState<BenchTableDelegate>>,
}

impl NativeBenchState {
    pub fn new(window: &mut Window, cx: &mut App) -> Self {
        let lang = active_language(cx);
        let u = tr(cx).utility();
        let ime_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(u.bench_ime_input_ph()));
        let ime_textarea = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(u.bench_ime_textarea_ph())
                .rows(4)
        });
        let ime_editor = cx.new(|cx| {
            let mut ed = EditorState::new(window, cx).default_value(
                "{\n  \"greeting\": \"Hello PingDeck GPUI\",\n  \"engine\": \"gpui-kit 0.11\"\n}",
            );
            ed.set_highlighter("json", cx);
            ed
        });
        let large_json_editor = cx.new(|cx| {
            let mut ed = EditorState::new(window, cx)
                .default_value("{\"hint\": \"Click 'Load 10 MiB JSON' to benchmark Tree-sitter + Rope memory & frame time\"}");
            ed.set_highlighter("json", cx);
            ed
        });
        let table_state = cx.new(|cx| TableState::new(BenchTableDelegate::new(10_000), window, cx));

        Self {
            tab: BenchTab::Ime,
            lang,
            ime_input,
            ime_textarea,
            ime_editor,
            large_json_editor,
            large_json_stats: "Ready (0 B)".into(),
            last_json_load_ms: None,
            stream_md: u.bench_stream_sample().to_owned(),
            stream_tokens: 0,
            table_state,
        }
    }

    pub fn load_10mib_json(&mut self, window: &mut Window, cx: &mut App) -> f64 {
        let start = Instant::now();
        let mut buf = String::with_capacity(10 * 1024 * 1024 + 1024);
        buf.push_str("{\n  \"benchmark\": \"pingdeck-10mib-json\",\n  \"items\": [\n");
        let row = "    {\"id\": 123456, \"provider\": \"openai\", \"model\": \"gpt-5.2\", \"latency_ms\": 42, \"ok\": true, \"note\": \"pure rust rope zero-ipc\"},\n";
        let target = 10 * 1024 * 1024;
        while buf.len() < target {
            buf.push_str(row);
        }
        buf.push_str("    {\"id\": 999999, \"end\": true}\n  ]\n}\n");
        let bytes = buf.len();
        self.large_json_editor.update(cx, |ed, cx| {
            ed.set_value(buf, window, cx);
        });
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        self.last_json_load_ms = Some(ms);
        self.large_json_stats = format!(
            "Loaded {:.2} MiB into Rope Editor in {:.1} ms",
            (bytes as f64) / (1024.0 * 1024.0),
            ms
        );
        ms
    }

    pub fn append_stream_burst(&mut self) {
        let u = tr_lang(self.lang).utility();
        for _ in 0..20 {
            self.stream_tokens += 10;
            self.stream_md
                .push_str(&u.bench_stream_burst_line(self.stream_tokens));
        }
    }

    pub fn render<V: 'static>(
        &self,
        cx: &mut Context<V>,
        on_select_tab: impl Fn(&mut V, BenchTab, &mut Context<V>) + Copy + 'static,
        on_burst: impl Fn(&mut V, &mut Context<V>) + Copy + 'static,
        on_load_10mb: impl Fn(&mut V, &mut Window, &mut Context<V>) + Copy + 'static,
    ) -> impl IntoElement {
        let p = palette(cx);
        let u = tr(cx).utility();
        let tab_btn =
            |id: &'static str, label: &'static str, tab: BenchTab, cx: &mut Context<V>| {
                if self.tab == tab {
                    btn_primary(id, label)
                } else {
                    btn_secondary(id, label)
                }
                .on_click(cx.listener(move |this, _, _, cx| on_select_tab(this, tab, cx)))
            };

        let tabs = h_row()
            .gap(px(6.0))
            .pb(px(10.0))
            .child(tab_btn("bench-ime", u.bench_tab_ime(), BenchTab::Ime, cx))
            .child(tab_btn(
                "bench-md",
                u.bench_tab_md(),
                BenchTab::StreamingMarkdown,
                cx,
            ))
            .child(tab_btn(
                "bench-tbl",
                u.bench_tab_table(),
                BenchTab::VirtualTable10k,
                cx,
            ))
            .child(tab_btn(
                "bench-json",
                u.bench_tab_json(),
                BenchTab::LargeJson10Mb,
                cx,
            ));

        let body = match self.tab {
            BenchTab::Ime => v_col()
                .flex_1()
                .gap(px(10.0))
                .child(section_heading(u.bench_sec_input(), cx))
                .child(text_input(&self.ime_input))
                .child(section_heading(u.bench_sec_textarea(), cx))
                .child(multiline_input(&self.ime_textarea))
                .child(section_heading(u.bench_sec_editor(), cx))
                .child(
                    div()
                        .flex_1()
                        .min_h(px(180.0))
                        .child(code_editor(&self.ime_editor)),
                ),
            BenchTab::StreamingMarkdown => v_col()
                .flex_1()
                .gap(px(10.0))
                .child(
                    h_row()
                        .justify_between()
                        .child(badge(
                            format!(
                                "Appended {} tokens (selectable + stream_fade)",
                                self.stream_tokens
                            ),
                            Tone::Accent,
                            cx,
                        ))
                        .child(
                            btn_primary("bench-md-burst", "+200 Tokens")
                                .on_click(cx.listener(move |this, _, _, cx| on_burst(this, cx))),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h(px(300.0))
                        .p(px(12.0))
                        .rounded(px(8.0))
                        .bg(to_hsla(p.pane))
                        .border_1()
                        .border_color(to_hsla(p.line.composite_over(p.pane)))
                        .overflow_hidden()
                        .child(streaming_markdown_view(
                            "bench-stream-md",
                            self.stream_md.clone(),
                        )),
                ),
            BenchTab::VirtualTable10k => v_col()
                .flex_1()
                .gap(px(8.0))
                .child(badge(
                    "10,000 virtualized rows · GPUI DataTable (120fps scroll)",
                    Tone::Success,
                    cx,
                ))
                .child(
                    div()
                        .flex_1()
                        .min_h(px(360.0))
                        .child(virtual_table(&self.table_state)),
                ),
            BenchTab::LargeJson10Mb => v_col()
                .flex_1()
                .gap(px(8.0))
                .child(
                    h_row()
                        .justify_between()
                        .child(badge(self.large_json_stats.clone(), Tone::Accent, cx))
                        .child(btn_primary("bench-load-10mb", "Load 10 MiB JSON").on_click(
                            cx.listener(move |this, _, window, cx| on_load_10mb(this, window, cx)),
                        )),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h(px(360.0))
                        .child(code_editor(&self.large_json_editor)),
                ),
        };

        v_col().size_full().child(tabs).child(body)
    }
}
