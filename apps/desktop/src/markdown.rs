//! Native GPUI rich Markdown renderer with true inline spans (`StyledText` + `InteractiveText`),
//! real font weights, comfortable line-height rhythm, clickable hyperlinks (`[text](url)` and
//! bare `https://`), and clickable inline workspace citations (`applications/...`, `briefs/...`,
//! `output/...`, `context/...`).
use crate::theme;
use gpui::{
    AnyElement, App, FontStyle, FontWeight, HighlightStyle, InteractiveText, IntoElement,
    ParentElement, SharedString, Styled, StyledText, UnderlineStyle, Window, div, px,
};
use std::{ops::Range, rc::Rc};

#[derive(Clone, Debug)]
pub enum LinkTarget {
    Url(String),
    WorkspacePath(String),
}

pub type LinkHandler = Rc<dyn Fn(LinkTarget, &mut Window, &mut App)>;

struct InlineParsed {
    text: String,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    click_ranges: Vec<Range<usize>>,
    click_targets: Vec<LinkTarget>,
}

/// Renders Markdown into clean, well-proportioned GPUI blocks with true inline bold/italic/code,
/// clickable URLs, and clickable workspace citations.
pub fn render_markdown(
    id_prefix: &str,
    source: &str,
    spacious: bool,
    on_link: LinkHandler,
) -> impl IntoElement {
    let mut blocks: Vec<AnyElement> = Vec::new();
    let mut in_code = false;
    let mut code_buf: Vec<String> = Vec::new();
    let mut block_ix: usize = 0;

    let body_size = if spacious { px(14.0) } else { px(12.5) };
    let body_lh = if spacious { px(22.0) } else { px(19.0) };

    for raw_line in source.lines() {
        let line = raw_line.trim();
        if line.starts_with("```") {
            if in_code {
                if !code_buf.is_empty() {
                    blocks.push(
                        div()
                            .my_1()
                            .p_3()
                            .rounded(px(8.0))
                            .bg(theme::window_glass())
                            .border_1()
                            .border_color(theme::hairline())
                            .text_size(if spacious { px(12.5) } else { px(11.5) })
                            .line_height(px(18.0))
                            .text_color(theme::text_secondary())
                            .child(code_buf.join("\n"))
                            .into_any_element(),
                    );
                    code_buf.clear();
                }
                in_code = false;
            } else {
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

        block_ix += 1;
        let el_id = SharedString::from(format!("{id_prefix}-b-{block_ix}"));

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
                    .child(render_inline(el_id, h1, on_link.clone()))
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
                    .child(render_inline(el_id, h2, on_link.clone()))
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
                    .child(render_inline(el_id, h3, on_link.clone()))
                    .into_any_element(),
            );
        } else if let Some(quote) = line.strip_prefix("> ") {
            blocks.push(
                div()
                    .pl_3()
                    .py_1()
                    .border_l_2()
                    .border_color(theme::hairline_strong())
                    .text_size(body_size)
                    .line_height(body_lh)
                    .text_color(theme::text_muted())
                    .child(render_inline(el_id, quote, on_link.clone()))
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
                        on_link.clone(),
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
                        on_link.clone(),
                    )))
                    .into_any_element(),
            );
        } else {
            blocks.push(
                div()
                    .text_size(body_size)
                    .line_height(body_lh)
                    .text_color(theme::text_secondary())
                    .child(render_inline(el_id, line, on_link.clone()))
                    .into_any_element(),
            );
        }
    }

    if in_code && !code_buf.is_empty() {
        blocks.push(
            div()
                .my_1()
                .p_3()
                .rounded(px(8.0))
                .bg(theme::window_glass())
                .border_1()
                .border_color(theme::hairline())
                .text_size(if spacious { px(12.5) } else { px(11.5) })
                .line_height(px(18.0))
                .text_color(theme::text_secondary())
                .child(code_buf.join("\n"))
                .into_any_element(),
        );
    }

    div()
        .flex()
        .flex_col()
        .gap(if spacious { px(10.0) } else { px(7.0) })
        .children(blocks)
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

fn render_inline(id: SharedString, raw: &str, on_link: LinkHandler) -> AnyElement {
    let parsed = parse_inline(raw);
    let styled = StyledText::new(parsed.text).with_highlights(parsed.highlights);
    if parsed.click_ranges.is_empty() {
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
    }
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
        || token.starts_with("context/"))
        && (token.ends_with(".md") || token.ends_with(".pdf") || token.ends_with(".json"))
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
