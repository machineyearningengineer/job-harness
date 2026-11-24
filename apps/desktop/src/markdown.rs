//! Native GPUI rich Markdown renderer with true inline spans (`StyledText` + `InteractiveText`),
//! real font weights, comfortable line-height rhythm, clickable hyperlinks (`[text](url)` and
//! bare `https://`), and clickable inline workspace citations (`applications/...`, `briefs/...`,
//! `output/...`, `context/...`).
use crate::theme;
use gpui::{
    AnyElement, App, FontStyle, FontWeight, HighlightStyle, InteractiveElement, InteractiveText,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement,
    SharedString, Styled, StyledText, UnderlineStyle, Window, div, px,
};
use std::{ops::Range, rc::Rc};

#[derive(Clone, Debug)]
pub enum LinkTarget {
    Url(String),
    WorkspacePath(String),
}

#[derive(Clone, Debug)]
pub enum TextSelectEvent {
    Start {
        block_id: SharedString,
        index: usize,
        full_text: String,
    },
    Move {
        block_id: SharedString,
        index: usize,
    },
    End,
}

#[derive(Clone, Debug)]
pub struct ActiveSelection {
    pub block_id: SharedString,
    pub start: usize,
    pub end: usize,
}

pub type LinkHandler = Rc<dyn Fn(LinkTarget, &mut Window, &mut App)>;
pub type SelectHandler = Rc<dyn Fn(TextSelectEvent, &mut Window, &mut App)>;

struct InlineParsed {
    text: String,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    click_ranges: Vec<Range<usize>>,
    click_targets: Vec<LinkTarget>,
}

/// Renders Markdown into clean, well-proportioned GPUI blocks with true inline bold/italic/code,
/// clickable URLs, clickable workspace citations, and native drag-to-select inline text.
pub fn render_markdown(
    id_prefix: &str,
    source: &str,
    spacious: bool,
    selection: Option<&ActiveSelection>,
    on_link: LinkHandler,
    on_select: SelectHandler,
) -> impl IntoElement {
    let mut blocks: Vec<AnyElement> = Vec::new();
    let mut in_code = false;
    let mut code_buf: Vec<String> = Vec::new();
    let mut block_ix: usize = 0;

    let body_size = if spacious { px(14.0) } else { px(12.5) };
    let body_lh = if spacious { px(22.0) } else { px(19.0) };

    let sel_for = |el_id: &SharedString| -> Option<Range<usize>> {
        selection.and_then(|s| {
            if &s.block_id == el_id && s.start != s.end {
                Some(s.start.min(s.end)..s.start.max(s.end))
            } else {
                None
            }
        })
    };

    let lines: Vec<&str> = source.lines().collect();
    let mut li = 0usize;
    let mut code_lang = String::new();

    while li < lines.len() {
        let raw_line = lines[li];
        let line = raw_line.trim();
        li += 1;

        if line.starts_with("```") {
            if in_code {
                if !code_buf.is_empty() {
                    let code_str = code_buf.join("\n");
                    block_ix += 1;
                    let code_id = SharedString::from(format!("{id_prefix}-code-{block_ix}"));
                    blocks.push(render_syntax_code(
                        code_id,
                        &code_str,
                        &code_lang,
                        spacious,
                        sel_for(&SharedString::from(format!("{id_prefix}-code-{block_ix}"))),
                        on_select.clone(),
                    ));
                    code_buf.clear();
                }
                code_lang.clear();
                in_code = false;
            } else {
                code_lang = line.trim_start_matches('`').trim().to_owned();
                in_code = true;
            }
            continue;
        }
        if in_code {
            code_buf.push(raw_line.to_owned());
            continue;
        }
        if line.is_empty() || line.starts_with("<!--") || line.ends_with("-->") {
            continue;
        }

        // Native GFM Table: header row + `|---|---|` separator + data rows
        if is_table_row(line) && li < lines.len() && is_table_separator(lines[li].trim()) {
            let headers = split_table_cells(line);
            li += 1; // skip separator row
            let mut rows: Vec<Vec<String>> = Vec::new();
            while li < lines.len() {
                let r = lines[li].trim();
                if r.is_empty() || !is_table_row(r) {
                    break;
                }
                rows.push(split_table_cells(r));
                li += 1;
            }
            let col_count = headers
                .len()
                .max(rows.iter().map(Vec::len).max().unwrap_or(0))
                .max(1);
            // Smart proportional column weights based on content length so prose columns get room!
            let mut weights: Vec<f32> = vec![12.0; col_count];
            for (ci, h) in headers.iter().enumerate() {
                weights[ci] = weights[ci].max(h.chars().count().min(32) as f32);
            }
            for r in &rows {
                for (ci, cell) in r.iter().enumerate() {
                    if ci < col_count {
                        let plain_len = cell.chars().count().min(120) as f32;
                        weights[ci] = (weights[ci] * 0.4 + plain_len * 0.6).max(weights[ci] * 0.7);
                    }
                }
            }
            let weights: Vec<f32> = weights.into_iter().map(|w| w.clamp(14.0, 96.0)).collect();

            let cell_size = if spacious { px(12.5) } else { px(11.5) };
            let cell_lh = if spacious { px(18.5) } else { px(16.5) };

            let mut table_el = div()
                .my_2()
                .w_full()
                .rounded(theme::r_sm())
                .bg(theme::window_glass())
                .border_1()
                .border_color(theme::hairline())
                .overflow_hidden()
                .flex()
                .flex_col();

            // Header row
            let mut hdr_row = div()
                .w_full()
                .bg(theme::surface_subtle())
                .border_b_1()
                .border_color(theme::hairline_strong())
                .flex();
            for (ci, w) in weights.iter().enumerate() {
                block_ix += 1;
                let cell_id = SharedString::from(format!("{id_prefix}-th-{block_ix}"));
                let cell_sel = sel_for(&cell_id);
                let txt = headers.get(ci).map(String::as_str).unwrap_or("");
                let mut cell = div()
                    .min_w_0()
                    .px_3()
                    .py_2()
                    .text_size(cell_size)
                    .line_height(cell_lh)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_primary())
                    .child(render_inline(
                        cell_id,
                        txt,
                        cell_sel,
                        on_link.clone(),
                        on_select.clone(),
                    ));
                if ci > 0 {
                    cell = cell.border_l_1().border_color(theme::hairline());
                }
                cell.style().flex_grow = Some(*w);
                cell.style().flex_basis = Some(px(0.0).into());
                hdr_row = hdr_row.child(cell);
            }
            table_el = table_el.child(hdr_row);

            // Body rows
            for (ri, r) in rows.iter().enumerate() {
                let mut row_el = div().w_full().flex();
                if ri > 0 {
                    row_el = row_el.border_t_1().border_color(theme::hairline());
                }
                if ri % 2 == 1 {
                    row_el = row_el.bg(gpui::rgba(0xffffff05));
                }
                for (ci, w) in weights.iter().enumerate() {
                    block_ix += 1;
                    let cell_id = SharedString::from(format!("{id_prefix}-td-{block_ix}"));
                    let cell_sel = sel_for(&cell_id);
                    let txt = r.get(ci).map(String::as_str).unwrap_or("");
                    let mut cell = div()
                        .min_w_0()
                        .px_3()
                        .py_2()
                        .text_size(cell_size)
                        .line_height(cell_lh)
                        .text_color(if ci == 0 {
                            theme::text_primary()
                        } else {
                            theme::text_secondary()
                        })
                        .child(render_inline(
                            cell_id,
                            txt,
                            cell_sel,
                            on_link.clone(),
                            on_select.clone(),
                        ));
                    if ci > 0 {
                        cell = cell.border_l_1().border_color(theme::hairline());
                    }
                    cell.style().flex_grow = Some(*w);
                    cell.style().flex_basis = Some(px(0.0).into());
                    row_el = row_el.child(cell);
                }
                table_el = table_el.child(row_el);
            }

            blocks.push(table_el.into_any_element());
            continue;
        }

        block_ix += 1;
        let el_id = SharedString::from(format!("{id_prefix}-b-{block_ix}"));
        let sel_range = sel_for(&el_id);

        if line == "---" || line == "***" || line == "___" {
            blocks.push(
                div()
                    .my_2()
                    .h(px(1.0))
                    .w_full()
                    .bg(theme::hairline())
                    .into_any_element(),
            );
        } else if let Some(h1) = line.strip_prefix("# ") {
            blocks.push(
                div()
                    .pt_2()
                    .text_size(if spacious { px(17.0) } else { px(14.5) })
                    .line_height(if spacious { px(24.0) } else { px(20.0) })
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_primary())
                    .child(render_inline(
                        el_id,
                        h1,
                        sel_range,
                        on_link.clone(),
                        on_select.clone(),
                    ))
                    .into_any_element(),
            );
        } else if let Some(h2) = line.strip_prefix("## ") {
            blocks.push(
                div()
                    .pt_3()
                    .pb_1()
                    .border_b_1()
                    .border_color(theme::hairline())
                    .text_size(if spacious { px(14.5) } else { px(13.0) })
                    .line_height(if spacious { px(21.0) } else { px(18.5) })
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_primary())
                    .child(render_inline(
                        el_id,
                        h2,
                        sel_range,
                        on_link.clone(),
                        on_select.clone(),
                    ))
                    .into_any_element(),
            );
        } else if let Some(h3) = line.strip_prefix("### ") {
            blocks.push(
                div()
                    .pt_2()
                    .text_size(if spacious { px(13.5) } else { px(12.5) })
                    .line_height(if spacious { px(20.0) } else { px(18.0) })
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::lavender())
                    .child(render_inline(
                        el_id,
                        h3,
                        sel_range,
                        on_link.clone(),
                        on_select.clone(),
                    ))
                    .into_any_element(),
            );
        } else if let Some(quote) = line.strip_prefix("> ") {
            let (accent, wash, text_part) = if let Some(rest) = quote.strip_prefix("[!WARNING]") {
                (theme::amber_accent(), theme::amber_wash(), rest.trim())
            } else if let Some(rest) = quote.strip_prefix("[!CAUTION]") {
                (theme::rose_text(), theme::rose_wash(), rest.trim())
            } else if let Some(rest) = quote
                .strip_prefix("[!NOTE]")
                .or_else(|| quote.strip_prefix("[!TIP]"))
                .or_else(|| quote.strip_prefix("[!IMPORTANT]"))
            {
                (theme::lavender(), theme::lavender_wash(), rest.trim())
            } else {
                (theme::hairline_strong(), gpui::rgba(0x00000000), quote)
            };
            blocks.push(
                div()
                    .pl_3()
                    .pr_2()
                    .py_1()
                    .rounded(theme::r_xs())
                    .bg(wash)
                    .border_l_2()
                    .border_color(accent)
                    .text_size(body_size)
                    .line_height(body_lh)
                    .text_color(theme::text_muted())
                    .child(render_inline(
                        el_id,
                        if text_part.is_empty() {
                            quote
                        } else {
                            text_part
                        },
                        sel_range,
                        on_link.clone(),
                        on_select.clone(),
                    ))
                    .into_any_element(),
            );
        } else if let Some((checked, task)) = split_task_item(line) {
            blocks.push(
                div()
                    .pl_1()
                    .flex()
                    .items_start()
                    .gap_2()
                    .text_size(body_size)
                    .line_height(body_lh)
                    .text_color(if checked {
                        theme::text_muted()
                    } else {
                        theme::text_secondary()
                    })
                    .child(
                        div()
                            .mt(px(3.0))
                            .w(px(13.0))
                            .h(px(13.0))
                            .flex_shrink_0()
                            .rounded(px(3.5))
                            .border_1()
                            .border_color(if checked {
                                theme::emerald()
                            } else {
                                theme::hairline_strong()
                            })
                            .bg(if checked {
                                gpui::rgba(0x34d39924)
                            } else {
                                gpui::rgba(0x00000000)
                            })
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(9.5))
                            .text_color(theme::emerald())
                            .child(if checked { "✓" } else { "" }),
                    )
                    .child(div().flex_1().min_w_0().child(render_inline(
                        el_id,
                        task,
                        sel_range,
                        on_link.clone(),
                        on_select.clone(),
                    )))
                    .into_any_element(),
            );
        } else if let Some(bullet) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            blocks.push(
                div()
                    .pl_1()
                    .flex()
                    .items_start()
                    .gap_2()
                    .text_size(body_size)
                    .line_height(body_lh)
                    .text_color(theme::text_secondary())
                    .child(div().text_color(theme::text_muted()).child("•"))
                    .child(div().flex_1().min_w_0().child(render_inline(
                        el_id,
                        bullet,
                        sel_range,
                        on_link.clone(),
                        on_select.clone(),
                    )))
                    .into_any_element(),
            );
        } else if let Some((num, rest)) = split_numbered_list(line) {
            blocks.push(
                div()
                    .pl_1()
                    .flex()
                    .items_start()
                    .gap_2()
                    .text_size(body_size)
                    .line_height(body_lh)
                    .text_color(theme::text_secondary())
                    .child(
                        div()
                            .text_color(theme::text_muted())
                            .child(format!("{num}.")),
                    )
                    .child(div().flex_1().min_w_0().child(render_inline(
                        el_id,
                        rest,
                        sel_range,
                        on_link.clone(),
                        on_select.clone(),
                    )))
                    .into_any_element(),
            );
        } else {
            blocks.push(
                div()
                    .min_w_0()
                    .text_size(body_size)
                    .line_height(body_lh)
                    .text_color(theme::text_secondary())
                    .child(render_inline(
                        el_id,
                        line,
                        sel_range,
                        on_link.clone(),
                        on_select.clone(),
                    ))
                    .into_any_element(),
            );
        }
    }

    if in_code && !code_buf.is_empty() {
        let code_str = code_buf.join("\n");
        block_ix += 1;
        let code_id = SharedString::from(format!("{id_prefix}-code-{block_ix}"));
        let code_sel = sel_for(&code_id);
        blocks.push(render_syntax_code(
            code_id, &code_str, &code_lang, spacious, code_sel, on_select,
        ));
    }

    div()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(if spacious { px(10.0) } else { px(7.0) })
        .children(blocks)
}

/// Lightweight zero-dependency syntax-highlighted code / JSON block with exact indentation
/// and native drag-to-select support.
pub fn render_syntax_code(
    id: SharedString,
    code: &str,
    lang: &str,
    spacious: bool,
    sel_range: Option<Range<usize>>,
    on_select: SelectHandler,
) -> AnyElement {
    let normalized = lang.trim().to_ascii_lowercase();
    let is_json = normalized == "json" || normalized == "jsonc";
    let mut highlights = if is_json {
        highlight_json(code)
    } else {
        highlight_generic_code(code)
    };

    if let Some(sr) = sel_range {
        let len = code.len();
        let s = sr.start.min(len);
        let e = sr.end.min(len);
        if s < e && code.is_char_boundary(s) && code.is_char_boundary(e) {
            highlights.push((
                s..e,
                HighlightStyle {
                    background_color: Some(theme::lavender_wash().into()),
                    color: Some(theme::text_primary().into()),
                    ..Default::default()
                },
            ));
            highlights.sort_by_key(|(r, _)| r.start);
        }
    }

    let styled = StyledText::new(code.to_owned()).with_highlights(highlights);
    let layout_down = styled.layout().clone();
    let layout_move = styled.layout().clone();
    let id_down = id.clone();
    let id_move = id;
    let full_text = code.to_owned();
    let on_sel_down = on_select.clone();
    let on_sel_move = on_select.clone();
    let on_sel_up = on_select;

    let mut card = div()
        .my_1()
        .w_full()
        .rounded(theme::r_sm())
        .bg(theme::window_glass())
        .border_1()
        .border_color(theme::hairline())
        .overflow_hidden()
        .flex()
        .flex_col();

    if !lang.trim().is_empty() {
        card = card.child(
            div()
                .px_3()
                .py(px(4.0))
                .bg(theme::surface_subtle())
                .border_b_1()
                .border_color(theme::hairline())
                .text_size(px(11.0))
                .text_color(theme::text_muted())
                .child(lang.trim().to_owned()),
        );
    }

    card.child(
        div()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .p_3()
            .cursor_text()
            .text_size(if spacious { px(12.5) } else { px(11.5) })
            .line_height(px(18.5))
            .text_color(theme::text_secondary())
            .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, window, cx| {
                let idx = match layout_down.index_for_position(ev.position) {
                    Ok(i) | Err(i) => i,
                };
                on_sel_down(
                    TextSelectEvent::Start {
                        block_id: id_down.clone(),
                        index: idx,
                        full_text: full_text.clone(),
                    },
                    window,
                    cx,
                );
            })
            .on_mouse_move(move |ev: &MouseMoveEvent, window, cx| {
                if ev.dragging() {
                    let idx = match layout_move.index_for_position(ev.position) {
                        Ok(i) | Err(i) => i,
                    };
                    on_sel_move(
                        TextSelectEvent::Move {
                            block_id: id_move.clone(),
                            index: idx,
                        },
                        window,
                        cx,
                    );
                }
            })
            .on_mouse_up(MouseButton::Left, move |_ev: &MouseUpEvent, window, cx| {
                on_sel_up(TextSelectEvent::End, window, cx);
            })
            .child(styled),
    )
    .into_any_element()
}

/// Renders parsed JSON into a clean, human-scannable structured view with colored types & sections.
pub fn render_json_structured(
    id_prefix: &str,
    val: &serde_json::Value,
    selection: Option<&ActiveSelection>,
    on_link: LinkHandler,
    on_select: SelectHandler,
) -> AnyElement {
    let mut counter = 0usize;
    render_json_node(
        id_prefix,
        val,
        0,
        &mut counter,
        selection,
        &on_link,
        &on_select,
    )
}

fn humanize_json_key(key: &str) -> String {
    let mut out = String::new();
    for (i, part) in key.split(['_', '-']).filter(|s| !s.is_empty()).enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            if i == 0 {
                out.extend(first.to_uppercase());
            } else {
                out.push(first);
            }
            out.push_str(chars.as_str());
        }
    }
    if out.is_empty() { key.to_owned() } else { out }
}

fn render_json_node(
    id_prefix: &str,
    val: &serde_json::Value,
    depth: usize,
    counter: &mut usize,
    selection: Option<&ActiveSelection>,
    on_link: &LinkHandler,
    on_select: &SelectHandler,
) -> AnyElement {
    let sel_for = |el_id: &SharedString| -> Option<Range<usize>> {
        selection.and_then(|s| {
            if &s.block_id == el_id && s.start != s.end {
                Some(s.start.min(s.end)..s.start.max(s.end))
            } else {
                None
            }
        })
    };

    match val {
        serde_json::Value::Object(map) => {
            let mut col = div()
                .flex()
                .flex_col()
                .gap(if depth == 0 { px(12.0) } else { px(6.0) });
            for (k, v) in map {
                let label = humanize_json_key(k);
                match v {
                    serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                        let is_empty = match v {
                            serde_json::Value::Array(a) => a.is_empty(),
                            serde_json::Value::Object(o) => o.is_empty(),
                            _ => false,
                        };
                        if is_empty {
                            continue;
                        }
                        let mut sec = div().flex().flex_col().gap_2();
                        if depth == 0 {
                            sec = sec.pt_2().child(
                                div()
                                    .pb_1()
                                    .border_b_1()
                                    .border_color(theme::hairline())
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text_primary())
                                    .child(label),
                            );
                        } else {
                            sec = sec.child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::lavender())
                                    .child(label),
                            );
                        }
                        sec = sec.child(render_json_node(
                            id_prefix,
                            v,
                            depth + 1,
                            counter,
                            selection,
                            on_link,
                            on_select,
                        ));
                        col = col.child(sec);
                    }
                    _ => {
                        *counter += 1;
                        let cid = SharedString::from(format!("{id_prefix}-jv-{counter}"));
                        let csel = sel_for(&cid);
                        let val_el = render_json_scalar(cid, v, csel, on_link, on_select);
                        col = col.child(
                            div()
                                .flex()
                                .items_start()
                                .gap_3()
                                .text_size(px(13.0))
                                .line_height(px(19.5))
                                .child(
                                    div()
                                        .w(px(132.0))
                                        .flex_shrink_0()
                                        .text_color(theme::text_muted())
                                        .child(label),
                                )
                                .child(div().flex_1().min_w_0().child(val_el)),
                        );
                    }
                }
            }
            col.into_any_element()
        }
        serde_json::Value::Array(items) => {
            let mut col = div().flex().flex_col().gap_2();
            for item in items {
                match item {
                    serde_json::Value::Object(_) => {
                        col = col.child(
                            div()
                                .p_3()
                                .rounded(theme::r_sm())
                                .bg(theme::window_glass())
                                .border_1()
                                .border_color(theme::hairline())
                                .child(render_json_node(
                                    id_prefix,
                                    item,
                                    depth + 1,
                                    counter,
                                    selection,
                                    on_link,
                                    on_select,
                                )),
                        );
                    }
                    _ => {
                        *counter += 1;
                        let cid = SharedString::from(format!("{id_prefix}-ja-{counter}"));
                        let csel = sel_for(&cid);
                        let val_el = render_json_scalar(cid, item, csel, on_link, on_select);
                        col = col.child(
                            div()
                                .pl_1()
                                .flex()
                                .items_start()
                                .gap_2()
                                .text_size(px(13.0))
                                .line_height(px(19.5))
                                .child(div().text_color(theme::text_muted()).child("•"))
                                .child(div().flex_1().min_w_0().child(val_el)),
                        );
                    }
                }
            }
            col.into_any_element()
        }
        _ => {
            *counter += 1;
            let cid = SharedString::from(format!("{id_prefix}-js-{counter}"));
            let csel = sel_for(&cid);
            render_json_scalar(cid, val, csel, on_link, on_select)
        }
    }
}

fn render_json_scalar(
    id: SharedString,
    val: &serde_json::Value,
    sel_range: Option<Range<usize>>,
    on_link: &LinkHandler,
    on_select: &SelectHandler,
) -> AnyElement {
    match val {
        serde_json::Value::String(s) => div()
            .text_color(theme::text_primary())
            .child(render_inline(
                id,
                s,
                sel_range,
                on_link.clone(),
                on_select.clone(),
            ))
            .into_any_element(),
        serde_json::Value::Number(n) => div()
            .text_color(theme::amber_text())
            .child(n.to_string())
            .into_any_element(),
        serde_json::Value::Bool(b) => div()
            .text_color(if *b {
                theme::emerald()
            } else {
                theme::rose_text()
            })
            .child(if *b { "true" } else { "false" })
            .into_any_element(),
        serde_json::Value::Null => div()
            .text_color(theme::text_muted())
            .child("null")
            .into_any_element(),
        _ => div().into_any_element(),
    }
}

fn highlight_json(src: &str) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    let key_style = HighlightStyle {
        color: Some(theme::lavender().into()),
        ..Default::default()
    };
    let str_style = HighlightStyle {
        color: Some(theme::emerald().into()),
        ..Default::default()
    };
    let num_style = HighlightStyle {
        color: Some(theme::amber_text().into()),
        ..Default::default()
    };
    let bool_style = HighlightStyle {
        color: Some(theme::rose_text().into()),
        ..Default::default()
    };
    let punct_style = HighlightStyle {
        color: Some(theme::text_muted().into()),
        ..Default::default()
    };

    while i < len {
        let b = bytes[i];
        if b == b'"' {
            let start = i;
            i += 1;
            while i < len {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(len);
                    continue;
                }
                if bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            let mut look = i;
            while look < len && bytes[look].is_ascii_whitespace() {
                look += 1;
            }
            let is_key = look < len && bytes[look] == b':';
            out.push((start..i, if is_key { key_style } else { str_style }));
        } else if b == b'-' || b.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < len
                && (bytes[i].is_ascii_digit()
                    || matches!(bytes[i], b'.' | b'e' | b'E' | b'+' | b'-'))
            {
                i += 1;
            }
            out.push((start..i, num_style));
        } else if src[i..].starts_with("true") {
            out.push((i..i + 4, bool_style));
            i += 4;
        } else if src[i..].starts_with("false") {
            out.push((i..i + 5, bool_style));
            i += 5;
        } else if src[i..].starts_with("null") {
            out.push((i..i + 4, punct_style));
            i += 4;
        } else if matches!(b, b'{' | b'}' | b'[' | b']' | b':' | b',') {
            out.push((i..i + 1, punct_style));
            i += 1;
        } else {
            i += src[i..].chars().next().map(char::len_utf8).unwrap_or(1);
        }
    }
    out
}

fn highlight_generic_code(src: &str) -> Vec<(Range<usize>, HighlightStyle)> {
    const KEYWORDS: &[&str] = &[
        "fn",
        "let",
        "mut",
        "const",
        "pub",
        "struct",
        "enum",
        "impl",
        "trait",
        "for",
        "while",
        "loop",
        "if",
        "else",
        "match",
        "return",
        "async",
        "await",
        "use",
        "mod",
        "type",
        "where",
        "def",
        "class",
        "import",
        "from",
        "as",
        "in",
        "not",
        "and",
        "or",
        "try",
        "except",
        "function",
        "export",
        "default",
        "interface",
        "true",
        "false",
        "null",
        "None",
        "True",
        "False",
        "SELECT",
        "FROM",
        "WHERE",
        "JOIN",
        "INSERT",
        "UPDATE",
        "DELETE",
        "ORDER",
        "BY",
    ];
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    let kw_style = HighlightStyle {
        color: Some(theme::lavender().into()),
        font_weight: Some(FontWeight::MEDIUM),
        ..Default::default()
    };
    let str_style = HighlightStyle {
        color: Some(theme::emerald().into()),
        ..Default::default()
    };
    let num_style = HighlightStyle {
        color: Some(theme::amber_text().into()),
        ..Default::default()
    };
    let comment_style = HighlightStyle {
        color: Some(theme::text_muted().into()),
        font_style: Some(FontStyle::Italic),
        ..Default::default()
    };

    while i < len {
        let b = bytes[i];
        if (b == b'/' && i + 1 < len && bytes[i + 1] == b'/')
            || b == b'#'
            || (b == b'-' && i + 1 < len && bytes[i + 1] == b'-')
        {
            let start = i;
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            out.push((start..i, comment_style));
        } else if b == b'"' || b == b'\'' || b == b'`' {
            let q = b;
            let start = i;
            i += 1;
            while i < len {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(len);
                    continue;
                }
                if bytes[i] == q {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push((start..i, str_style));
        } else if b.is_ascii_digit() {
            let start = i;
            while i < len
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.' || bytes[i] == b'_')
            {
                i += 1;
            }
            out.push((start..i, num_style));
        } else if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            if KEYWORDS.contains(&&src[start..i]) {
                out.push((start..i, kw_style));
            }
        } else {
            i += src[i..].chars().next().map(char::len_utf8).unwrap_or(1);
        }
    }
    out
}

fn is_table_row(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.ends_with('|') && t.len() >= 3 && t.matches('|').count() >= 2
}

fn is_table_separator(line: &str) -> bool {
    if !is_table_row(line) {
        return false;
    }
    let cells = split_table_cells(line);
    !cells.is_empty()
        && cells.iter().all(|c| {
            let s = c.trim();
            !s.is_empty()
                && s.chars().all(|ch| ch == '-' || ch == ':' || ch == ' ')
                && s.contains('-')
        })
}

fn split_table_cells(line: &str) -> Vec<String> {
    let inner = line.trim().trim_start_matches('|').trim_end_matches('|');
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut esc = false;
    for ch in inner.chars() {
        if esc {
            cur.push(ch);
            esc = false;
        } else if ch == '\\' {
            esc = true;
        } else if ch == '|' {
            cells.push(cur.trim().to_owned());
            cur.clear();
        } else {
            cur.push(ch);
        }
    }
    cells.push(cur.trim().to_owned());
    cells
}

fn split_task_item(line: &str) -> Option<(bool, &str)> {
    let rest = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))?;
    if let Some(t) = rest.strip_prefix("[ ] ") {
        Some((false, t))
    } else if let Some(t) = rest
        .strip_prefix("[x] ")
        .or_else(|| rest.strip_prefix("[X] "))
    {
        Some((true, t))
    } else {
        None
    }
}

fn split_numbered_list(line: &str) -> Option<(&str, &str)> {
    let dot = line.find(". ")?;
    if dot == 0 || dot > 3 {
        return None;
    }
    let prefix = &line[..dot];
    if prefix.chars().all(|c| c.is_ascii_digit()) {
        Some((prefix, line[dot + 2..].trim_start()))
    } else {
        None
    }
}

fn render_inline(
    id: SharedString,
    raw: &str,
    sel_range: Option<Range<usize>>,
    on_link: LinkHandler,
    on_select: SelectHandler,
) -> AnyElement {
    let mut parsed = parse_inline(raw);
    let full_text = parsed.text.clone();
    if let Some(sr) = sel_range {
        let len = parsed.text.len();
        let s = sr.start.min(len);
        let e = sr.end.min(len);
        if s < e && parsed.text.is_char_boundary(s) && parsed.text.is_char_boundary(e) {
            parsed.highlights.push((
                s..e,
                HighlightStyle {
                    background_color: Some(theme::lavender_wash().into()),
                    color: Some(theme::text_primary().into()),
                    ..Default::default()
                },
            ));
            parsed.highlights.sort_by_key(|(r, _)| r.start);
        }
    }
    let styled = StyledText::new(parsed.text).with_highlights(parsed.highlights);
    let layout_down = styled.layout().clone();
    let layout_move = styled.layout().clone();
    let id_down = id.clone();
    let id_move = id.clone();
    let on_sel_down = on_select.clone();
    let on_sel_move = on_select.clone();
    let on_sel_up = on_select;

    let inner: AnyElement = if parsed.click_ranges.is_empty() {
        styled.into_any_element()
    } else {
        let targets = parsed.click_targets;
        InteractiveText::new(id, styled)
            .on_click(parsed.click_ranges, move |ix, window, cx| {
                if let Some(target) = targets.get(ix).cloned() {
                    on_link(target, window, cx);
                }
            })
            .into_any_element()
    };

    div()
        .min_w_0()
        .cursor_text()
        .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, window, cx| {
            let idx = match layout_down.index_for_position(ev.position) {
                Ok(i) | Err(i) => i,
            };
            on_sel_down(
                TextSelectEvent::Start {
                    block_id: id_down.clone(),
                    index: idx,
                    full_text: full_text.clone(),
                },
                window,
                cx,
            );
        })
        .on_mouse_move(move |ev: &MouseMoveEvent, window, cx| {
            if ev.dragging() {
                let idx = match layout_move.index_for_position(ev.position) {
                    Ok(i) | Err(i) => i,
                };
                on_sel_move(
                    TextSelectEvent::Move {
                        block_id: id_move.clone(),
                        index: idx,
                    },
                    window,
                    cx,
                );
            }
        })
        .on_mouse_up(MouseButton::Left, move |_ev: &MouseUpEvent, window, cx| {
            on_sel_up(TextSelectEvent::End, window, cx);
        })
        .child(inner)
        .into_any_element()
}

fn parse_inline(raw: &str) -> InlineParsed {
    let mut out = String::with_capacity(raw.len());
    let mut highlights: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    let mut click_ranges: Vec<Range<usize>> = Vec::new();
    let mut click_targets: Vec<LinkTarget> = Vec::new();

    let link_style = HighlightStyle {
        color: Some(theme::lavender().into()),
        underline: Some(UnderlineStyle {
            thickness: px(1.0),
            color: Some(theme::lavender_border().into()),
            wavy: false,
        }),
        ..Default::default()
    };
    let bold_style = HighlightStyle {
        color: Some(theme::text_primary().into()),
        font_weight: Some(FontWeight::SEMIBOLD),
        ..Default::default()
    };
    let italic_style = HighlightStyle {
        font_style: Some(FontStyle::Italic),
        ..Default::default()
    };
    let code_style = HighlightStyle {
        color: Some(theme::text_primary().into()),
        background_color: Some(theme::surface_active().into()),
        ..Default::default()
    };

    let bytes = raw.as_bytes();
    let len = raw.len();
    let mut i = 0;

    while i < len {
        // 1. Markdown hyperlink: [label](url_or_path)
        if bytes[i] == b'['
            && let Some(close_br) = raw[i + 1..].find(']')
        {
            let label_end = i + 1 + close_br;
            if label_end + 1 < len
                && bytes[label_end + 1] == b'('
                && let Some(close_par) = raw[label_end + 2..].find(')')
            {
                let label = &raw[i + 1..label_end];
                let dest = raw[label_end + 2..label_end + 2 + close_par].trim();
                if !label.is_empty() && !dest.is_empty() {
                    let start = out.len();
                    out.push_str(label);
                    let end = out.len();
                    let target = if dest.starts_with("http://") || dest.starts_with("https://") {
                        LinkTarget::Url(dest.to_owned())
                    } else {
                        LinkTarget::WorkspacePath(dest.to_owned())
                    };
                    highlights.push((start..end, link_style));
                    click_ranges.push(start..end);
                    click_targets.push(target);
                    i = label_end + 2 + close_par + 1;
                    continue;
                }
            }
        }

        // 2. Bold: **text**
        if raw[i..].starts_with("**")
            && let Some(end_rel) = raw[i + 2..].find("**")
            && end_rel > 0
        {
            let inner = &raw[i + 2..i + 2 + end_rel];
            let start = out.len();
            out.push_str(inner);
            let end = out.len();
            highlights.push((start..end, bold_style));
            i = i + 2 + end_rel + 2;
            continue;
        }

        // 3. Inline code: `code` (automatically turns workspace paths inside backticks into inline links!)
        if bytes[i] == b'`'
            && let Some(end_rel) = raw[i + 1..].find('`')
            && end_rel > 0
        {
            let inner = &raw[i + 1..i + 1 + end_rel];
            let start = out.len();
            if is_workspace_citation(inner) {
                let short = inner.rsplit('/').next().unwrap_or(inner);
                out.push_str(short);
                let end = out.len();
                highlights.push((start..end, link_style));
                click_ranges.push(start..end);
                click_targets.push(LinkTarget::WorkspacePath(inner.to_owned()));
            } else {
                out.push_str(inner);
                let end = out.len();
                highlights.push((start..end, code_style));
            }
            i = i + 1 + end_rel + 1;
            continue;
        }

        // 4. Italic: *text* (only when not preceded by whitespace-free word and closed cleanly)
        if bytes[i] == b'*'
            && !raw[i..].starts_with("**")
            && let Some(end_rel) = raw[i + 1..].find('*')
            && end_rel > 0
            && !raw[i + 1..i + 1 + end_rel].starts_with(' ')
        {
            let inner = &raw[i + 1..i + 1 + end_rel];
            let start = out.len();
            out.push_str(inner);
            let end = out.len();
            highlights.push((start..end, italic_style));
            i = i + 1 + end_rel + 1;
            continue;
        }

        // 5. Bare URL: https://... or http://...
        if raw[i..].starts_with("https://") || raw[i..].starts_with("http://") {
            let token_end = raw[i..]
                .find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '>' | '"'))
                .map(|r| i + r)
                .unwrap_or(len);
            let url = raw[i..token_end].trim_end_matches(['.', ',', ';']);
            let start = out.len();
            let display = shorten_url_display(url);
            out.push_str(&display);
            let end = out.len();
            highlights.push((start..end, link_style));
            click_ranges.push(start..end);
            click_targets.push(LinkTarget::Url(url.to_owned()));
            i += url.len();
            continue;
        }

        // 6. Bare workspace citation path (`applications/...`, `briefs/...`, `output/...`, `context/...`)
        if (raw[i..].starts_with("applications/")
            || raw[i..].starts_with("briefs/")
            || raw[i..].starts_with("output/")
            || raw[i..].starts_with("context/"))
            && (i == 0
                || raw[..i]
                    .chars()
                    .last()
                    .is_some_and(|c| c.is_whitespace() || matches!(c, '(' | '[' | ':' | '/')))
        {
            let token_end = raw[i..]
                .find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '>' | '"' | '\''))
                .map(|r| i + r)
                .unwrap_or(len);
            let path = raw[i..token_end].trim_end_matches(['.', ',', ';', ':']);
            if is_workspace_citation(path) {
                let start = out.len();
                let short = path.rsplit('/').next().unwrap_or(path);
                out.push_str(short);
                let end = out.len();
                highlights.push((start..end, link_style));
                click_ranges.push(start..end);
                click_targets.push(LinkTarget::WorkspacePath(path.to_owned()));
                i += path.len();
                continue;
            }
        }

        // Regular UTF-8 character
        let ch = raw[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    InlineParsed {
        text: out,
        highlights,
        click_ranges,
        click_targets,
    }
}

fn is_workspace_citation(token: &str) -> bool {
    (token.starts_with("applications/")
        || token.starts_with("briefs/")
        || token.starts_with("output/")
        || token.starts_with("context/")
        || token.starts_with("content/"))
        && (token.ends_with(".md")
            || token.ends_with(".pdf")
            || token.ends_with(".json")
            || token.ends_with(".txt"))
}

fn shorten_url_display(url: &str) -> String {
    let stripped = url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.");
    let count = stripped.chars().count();
    if count <= 36 {
        stripped.to_owned()
    } else {
        let head: String = stripped.chars().take(35).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inline_bold_links_and_workspace_citations() {
        let parsed = parse_inline(
            "**Gigaton:** check applications/briefs/gigaton-screen.md and [Meet](https://meet.google.com/abc).",
        );
        assert_eq!(parsed.text, "Gigaton: check gigaton-screen.md and Meet.");
        assert_eq!(parsed.click_targets.len(), 2);
        assert!(matches!(
            &parsed.click_targets[0],
            LinkTarget::WorkspacePath(p) if p == "applications/briefs/gigaton-screen.md"
        ));
        assert!(matches!(
            &parsed.click_targets[1],
            LinkTarget::Url(u) if u == "https://meet.google.com/abc"
        ));
    }
}
