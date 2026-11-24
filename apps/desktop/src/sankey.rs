//! Museum-grade interactive native GPUI Sankey funnel view.
//! - Razor-thin 3px architectural node pins with crisp optical separation from ribbons
//! - Exact cubic-Bezier per-ribbon hit testing (hover/click directly on curved flows!)
//! - Non-overlapping labels (middle column labels anchored cleanly above/below pins)
//! - Floating scrollable drill-down popover with smooth entrance animation & instant exit
use crate::{theme, transport::RoleCard};
use gpui::{
    Animation, AnimationExt, AnyElement, Bounds, Context, FontWeight, InteractiveElement,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement, PathBuilder, Pixels,
    SharedString, StatefulInteractiveElement, Styled, Window, canvas, div, ease_out_quint, point,
    prelude::FluentBuilder, px, rgba,
};
use std::{cell::RefCell, rc::Rc, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SankeyBucket {
    Total,
    Saved,
    Submitted,
    Awaiting,
    Screening,
    Interviews,
    Technical,
    FinalRound,
    Offer,
    Rejected,
    Withdrawn,
    ClosedOther,
}

impl SankeyBucket {
    pub fn label(self) -> &'static str {
        match self {
            Self::Total => "All roles",
            Self::Saved => "Saved",
            Self::Submitted => "Submitted",
            Self::Awaiting => "Applied",
            Self::Screening => "Screening",
            Self::Interviews => "Interviews",
            Self::Technical => "Technical",
            Self::FinalRound => "Final",
            Self::Offer => "Offer",
            Self::Rejected => "Rejected",
            Self::Withdrawn => "Withdrawn",
            Self::ClosedOther => "Archived",
        }
    }

    /// Refined, high-contrast Linear/Stripe-inspired dark-mode palette.
    pub fn color(self) -> gpui::Rgba {
        match self {
            Self::Total => rgba(0xe4e4e7ff),
            Self::Saved => rgba(0x94a3b8ff),
            Self::Submitted => rgba(0x7dd3fcff),
            Self::Awaiting => rgba(0x38bdf8ff),
            Self::Screening => rgba(0x818cf8ff),
            Self::Interviews => rgba(0xfbbf24ff),
            Self::Technical => rgba(0xf59e0bff),
            Self::FinalRound => rgba(0xfde047ff),
            Self::Offer => rgba(0x34d399ff),
            Self::Rejected => rgba(0xfb7185ff),
            Self::Withdrawn => rgba(0xa1a1aaff),
            Self::ClosedOther => rgba(0x71717aff),
        }
    }

    /// True when tapping this node zooms into a focused child-stage Sankey with breadcrumbs.
    pub fn has_children(self) -> bool {
        matches!(self, Self::Submitted | Self::Interviews)
    }

    fn with_alpha(self, alpha: u8) -> gpui::Rgba {
        let base = self.color();
        let r = (base.r * 255.0) as u32;
        let g = (base.g * 255.0) as u32;
        let b = (base.b * 255.0) as u32;
        rgba((r << 24) | (g << 16) | (b << 8) | (alpha as u32))
    }
}

pub fn classify_card_leaf(card: &RoleCard) -> SankeyBucket {
    if card.column == "Archive" || card.stage == "closed" {
        match card
            .outcome
            .as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("rejected" | "rejection") => SankeyBucket::Rejected,
            Some("withdrawn" | "declined") => SankeyBucket::Withdrawn,
            _ => SankeyBucket::ClosedOther,
        }
    } else if card.stage == "offer" {
        SankeyBucket::Offer
    } else if matches!(card.stage.as_str(), "technical" | "final") || card.column == "Interviews" {
        SankeyBucket::Interviews
    } else if matches!(card.stage.as_str(), "screening" | "introduction")
        || card.column == "Screening"
    {
        SankeyBucket::Screening
    } else if card.stage == "applied" || card.column == "Applied" {
        SankeyBucket::Awaiting
    } else {
        SankeyBucket::Saved
    }
}

pub fn matches_bucket(card: &RoleCard, bucket: SankeyBucket) -> bool {
    let leaf = classify_card_leaf(card);
    match bucket {
        SankeyBucket::Total => true,
        SankeyBucket::Submitted => leaf != SankeyBucket::Saved,
        SankeyBucket::Technical => card.stage == "technical",
        SankeyBucket::FinalRound => card.stage == "final",
        other => leaf == other,
    }
}

#[derive(Clone, Copy)]
struct NodeGeom {
    bucket: SankeyBucket,
    count: usize,
    y: f32,
    h: f32,
}

#[derive(Clone, Copy)]
struct RibbonGeom {
    target: SankeyBucket,
    is_stage0: bool,
    x0: f32,
    y0_top: f32,
    y0_bot: f32,
    x1: f32,
    y1_top: f32,
    y1_bot: f32,
}

impl RibbonGeom {
    /// Evaluates the cubic Bezier ribbon's vertical span `[top, bot]` at screen `x`.
    fn y_span_at(&self, x: f32) -> Option<(f32, f32)> {
        if x < self.x0 || x > self.x1 || (self.x1 - self.x0) < 1.0 {
            return None;
        }
        // Horizontal midpoint Bezier has smooth S-curve easing S(u) = 3u^2 - 2u^3
        let u = ((x - self.x0) / (self.x1 - self.x0)).clamp(0.0, 1.0);
        let s = u * u * (3.0 - 2.0 * u);
        let top = self.y0_top + (self.y1_top - self.y0_top) * s;
        let bot = self.y0_bot + (self.y1_bot - self.y0_bot) * s;
        Some((top.min(bot), top.max(bot)))
    }
}

fn layout_column(items: &[(SankeyBucket, usize)], total_h: f32, gap: f32) -> Vec<NodeGeom> {
    let nonzero: Vec<(SankeyBucket, usize)> =
        items.iter().copied().filter(|(_, c)| *c > 0).collect();
    if nonzero.is_empty() {
        return Vec::new();
    }
    let sum: usize = nonzero.iter().map(|(_, c)| *c).sum();
    let total_gap = gap * (nonzero.len().saturating_sub(1) as f32);
    let avail = (total_h - total_gap).max(60.0);
    let min_h = 22.0_f32;
    let raw_hs: Vec<f32> = nonzero
        .iter()
        .map(|(_, c)| ((*c as f32 / sum.max(1) as f32) * avail).max(min_h))
        .collect();
    let raw_sum: f32 = raw_hs.iter().sum();
    let scale = if raw_sum > avail && raw_sum > 0.0 {
        avail / raw_sum
    } else {
        1.0
    };
    let used_h = raw_sum * scale + total_gap;
    let mut y = ((total_h - used_h) * 0.5).max(0.0);
    let mut out = Vec::with_capacity(nonzero.len());
    for ((bucket, count), rh) in nonzero.into_iter().zip(raw_hs) {
        let h = (rh * scale).max(14.0);
        out.push(NodeGeom {
            bucket,
            count,
            y,
            h,
        });
        y += h + gap;
    }
    out
}

fn build_ribbons(
    bounds: Bounds<Pixels>,
    col0: &[NodeGeom],
    col1: &[NodeGeom],
    col2: &[NodeGeom],
    zoom_root: Option<SankeyBucket>,
) -> (f32, f32, f32, Vec<RibbonGeom>) {
    let w: f32 = bounds.size.width.into();
    let ox: f32 = bounds.origin.x.into();
    let oy: f32 = bounds.origin.y.into();
    let pin_w = 3.0_f32;
    let gap_x = 3.0_f32;
    let x_col0 = ox + 108.0;
    let x_col1 = ox + w * 0.42;
    let x_col2 = ox + w - 186.0;
    let mut ribbons: Vec<RibbonGeom> = Vec::new();
    if w < 220.0 {
        return (x_col0, x_col1, x_col2, ribbons);
    }

    // Zoomed 2-column mode (parent -> children directly!)
    if zoom_root.is_some() {
        if let Some(root) = col0.first() {
            let mut src_y = oy + root.y;
            let leaf_sum = col2.iter().map(|n| n.count).sum::<usize>().max(1);
            for target in col2 {
                let frac = target.count as f32 / leaf_sum as f32;
                let band_h = root.h * frac;
                ribbons.push(RibbonGeom {
                    target: target.bucket,
                    is_stage0: false,
                    x0: x_col0 + pin_w + gap_x,
                    y0_top: src_y,
                    y0_bot: src_y + band_h,
                    x1: x_col2 - gap_x,
                    y1_top: oy + target.y,
                    y1_bot: oy + target.y + target.h,
                });
                src_y += band_h;
            }
        }
        return (x_col0, x_col1, x_col2, ribbons);
    }

    if let Some(root) = col0.first() {
        let mut src_y = oy + root.y;
        let root_sum = col1.iter().map(|n| n.count).sum::<usize>().max(1);
        for target in col1 {
            let frac = target.count as f32 / root_sum as f32;
            let band_h = root.h * frac;
            ribbons.push(RibbonGeom {
                target: target.bucket,
                is_stage0: true,
                x0: x_col0 + pin_w + gap_x,
                y0_top: src_y,
                y0_bot: src_y + band_h,
                x1: x_col1 - gap_x,
                y1_top: oy + target.y,
                y1_bot: oy + target.y + target.h,
            });
            src_y += band_h;
        }
    }

    if let Some(sub_node) = col1.iter().find(|n| n.bucket == SankeyBucket::Submitted) {
        let mut src_y = oy + sub_node.y;
        let sub_sum = col2.iter().map(|n| n.count).sum::<usize>().max(1);
        for target in col2 {
            let frac = target.count as f32 / sub_sum as f32;
            let band_h = sub_node.h * frac;
            ribbons.push(RibbonGeom {
                target: target.bucket,
                is_stage0: false,
                x0: x_col1 + pin_w + gap_x,
                y0_top: src_y,
                y0_bot: src_y + band_h,
                x1: x_col2 - gap_x,
                y1_top: oy + target.y,
                y1_bot: oy + target.y + target.h,
            });
            src_y += band_h;
        }
    }
    (x_col0, x_col1, x_col2, ribbons)
}

/// Renders the full-canvas interactive Sankey funnel with exact per-ribbon hover,
/// hairline 3px pins, hierarchical parent->child zoom with breadcrumbs, and a sleek
/// scrollable floating drill-down popover.
#[allow(clippy::too_many_arguments)]
pub fn render_sankey_view<V: 'static>(
    cards: &[RoleCard],
    zoom_root: Option<SankeyBucket>,
    selected: Option<SankeyBucket>,
    hovered: Option<SankeyBucket>,
    anim_epoch: u64,
    reduce_motion: bool,
    on_hover: impl Fn(Option<SankeyBucket>, &mut V, &mut Window, &mut Context<V>) + Clone + 'static,
    on_zoom: impl Fn(Option<SankeyBucket>, &mut V, &mut Window, &mut Context<V>) + Clone + 'static,
    on_select: impl Fn(Option<SankeyBucket>, &mut V, &mut Window, &mut Context<V>) + Clone + 'static,
    on_open_role: impl Fn(RoleCard, &mut V, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let total = cards.len();
    let mut saved = 0usize;
    let mut awaiting = 0usize;
    let mut screening = 0usize;
    let mut interviews = 0usize;
    let mut technical = 0usize;
    let mut final_round = 0usize;
    let mut offer = 0usize;
    let mut rejected = 0usize;
    let mut withdrawn = 0usize;
    let mut closed_other = 0usize;

    for c in cards {
        if c.stage == "technical" {
            technical += 1;
        } else if c.stage == "final" {
            final_round += 1;
        }
        match classify_card_leaf(c) {
            SankeyBucket::Saved => saved += 1,
            SankeyBucket::Awaiting => awaiting += 1,
            SankeyBucket::Screening => screening += 1,
            SankeyBucket::Interviews => interviews += 1,
            SankeyBucket::Offer => offer += 1,
            SankeyBucket::Rejected => rejected += 1,
            SankeyBucket::Withdrawn => withdrawn += 1,
            SankeyBucket::ClosedOther => closed_other += 1,
            SankeyBucket::Total
            | SankeyBucket::Submitted
            | SankeyBucket::Technical
            | SankeyBucket::FinalRound => {}
        }
    }
    let submitted = total.saturating_sub(saved);
    let focus_bucket = hovered.or(selected);
    let denom = match zoom_root {
        Some(SankeyBucket::Interviews) => interviews.max(1),
        Some(SankeyBucket::Submitted) | None => submitted.max(1),
        _ => total.max(1),
    };

    let diagram_h = 440.0_f32;
    let top_pad = 34.0_f32;

    let (col0, col1, col2) = match zoom_root {
        Some(SankeyBucket::Interviews) => {
            let c0: Vec<NodeGeom> = layout_column(
                &[(SankeyBucket::Interviews, interviews)],
                diagram_h * 0.60,
                16.0,
            )
            .into_iter()
            .map(|mut n| {
                n.y += diagram_h * 0.20;
                n
            })
            .collect();
            let c2: Vec<NodeGeom> = layout_column(
                &[
                    (SankeyBucket::Technical, technical),
                    (SankeyBucket::FinalRound, final_round),
                ],
                diagram_h * 0.68,
                28.0,
            )
            .into_iter()
            .map(|mut n| {
                n.y += diagram_h * 0.16;
                n
            })
            .collect();
            (c0, Vec::new(), c2)
        }
        Some(SankeyBucket::Submitted) => {
            let c0: Vec<NodeGeom> = layout_column(
                &[(SankeyBucket::Submitted, submitted)],
                diagram_h * 0.78,
                16.0,
            )
            .into_iter()
            .map(|mut n| {
                n.y += diagram_h * 0.11;
                n
            })
            .collect();
            let c2 = layout_column(
                &[
                    (SankeyBucket::Offer, offer),
                    (SankeyBucket::Interviews, interviews),
                    (SankeyBucket::Screening, screening),
                    (SankeyBucket::Awaiting, awaiting),
                    (SankeyBucket::Rejected, rejected),
                    (SankeyBucket::Withdrawn, withdrawn),
                    (SankeyBucket::ClosedOther, closed_other),
                ],
                diagram_h,
                14.0,
            );
            (c0, Vec::new(), c2)
        }
        _ => {
            let c0: Vec<NodeGeom> =
                layout_column(&[(SankeyBucket::Total, total)], diagram_h * 0.72, 16.0)
                    .into_iter()
                    .map(|mut n| {
                        n.y += diagram_h * 0.14;
                        n
                    })
                    .collect();
            let c1: Vec<NodeGeom> = layout_column(
                &[
                    (SankeyBucket::Submitted, submitted),
                    (SankeyBucket::Saved, saved),
                ],
                diagram_h * 0.84,
                34.0,
            )
            .into_iter()
            .map(|mut n| {
                n.y += diagram_h * 0.08;
                n
            })
            .collect();
            let c2 = layout_column(
                &[
                    (SankeyBucket::Offer, offer),
                    (SankeyBucket::Interviews, interviews),
                    (SankeyBucket::Screening, screening),
                    (SankeyBucket::Awaiting, awaiting),
                    (SankeyBucket::Rejected, rejected),
                    (SankeyBucket::Withdrawn, withdrawn),
                    (SankeyBucket::ClosedOther, closed_other),
                ],
                diagram_h,
                14.0,
            );
            (c0, c1, c2)
        }
    };

    let shared_bounds: Rc<RefCell<Option<Bounds<Pixels>>>> = Rc::new(RefCell::new(None));
    let bounds_for_paint = shared_bounds.clone();
    let col0_paint = col0.clone();
    let col1_paint = col1.clone();
    let col2_paint = col2.clone();
    let sel_paint = focus_bucket;

    let ribbon_canvas = canvas(
        move |bounds: Bounds<Pixels>, _window: &mut Window, _cx: &mut gpui::App| {
            *bounds_for_paint.borrow_mut() = Some(bounds);
        },
        move |bounds: Bounds<Pixels>, _, window: &mut Window, _cx: &mut gpui::App| {
            let pin_w = 3.0_f32;
            let oy: f32 = bounds.origin.y.into();
            let (x_col0, x_col1, x_col2, ribbons) =
                build_ribbons(bounds, &col0_paint, &col1_paint, &col2_paint, zoom_root);
            if ribbons.is_empty() {
                return;
            }

            // Paint smooth cubic-Bezier ribbons with per-path contrast & luminous highlight
            for r in &ribbons {
                let is_direct = sel_paint == Some(r.target);
                let is_ancestor = r.is_stage0
                    && r.target == SankeyBucket::Submitted
                    && sel_paint.is_some_and(|s| {
                        !matches!(
                            s,
                            SankeyBucket::Total | SankeyBucket::Saved | SankeyBucket::Submitted
                        )
                    });
                let is_descendant = !r.is_stage0 && sel_paint == Some(SankeyBucket::Submitted);
                let lit = is_direct || is_descendant;
                let dimmed = sel_paint.is_some()
                    && sel_paint != Some(SankeyBucket::Total)
                    && !lit
                    && !is_ancestor;

                // Pick the color: when hovering a specific leaf, tint the upstream trunk subtly too!
                let tint_bucket = if is_ancestor {
                    sel_paint.unwrap_or(r.target)
                } else {
                    r.target
                };
                let mid_x = (r.x0 + r.x1) * 0.5;

                // Soft luminous halo ONLY on actively highlighted ribbon
                if lit {
                    let halo = 3.5_f32;
                    let mut gp = PathBuilder::fill();
                    gp.move_to(point(px(r.x0), px(r.y0_top - halo)));
                    gp.cubic_bezier_to(
                        point(px(r.x1), px(r.y1_top - halo)),
                        point(px(mid_x), px(r.y0_top - halo)),
                        point(px(mid_x), px(r.y1_top - halo)),
                    );
                    gp.line_to(point(px(r.x1), px(r.y1_bot + halo)));
                    gp.cubic_bezier_to(
                        point(px(r.x0), px(r.y0_bot + halo)),
                        point(px(mid_x), px(r.y1_bot + halo)),
                        point(px(mid_x), px(r.y0_bot + halo)),
                    );
                    gp.line_to(point(px(r.x0), px(r.y0_top - halo)));
                    if let Ok(path) = gp.build() {
                        window.paint_path(path, tint_bucket.with_alpha(0x1c));
                    }
                }

                // Main sculpted Bezier ribbon
                let mut pb = PathBuilder::fill();
                pb.move_to(point(px(r.x0), px(r.y0_top)));
                pb.cubic_bezier_to(
                    point(px(r.x1), px(r.y1_top)),
                    point(px(mid_x), px(r.y0_top)),
                    point(px(mid_x), px(r.y1_top)),
                );
                pb.line_to(point(px(r.x1), px(r.y1_bot)));
                pb.cubic_bezier_to(
                    point(px(r.x0), px(r.y0_bot)),
                    point(px(mid_x), px(r.y1_bot)),
                    point(px(mid_x), px(r.y0_bot)),
                );
                pb.line_to(point(px(r.x0), px(r.y0_top)));
                if let Ok(path) = pb.build() {
                    let alpha = if lit {
                        0x5e
                    } else if is_ancestor {
                        0x28
                    } else if dimmed {
                        0x0d
                    } else if r.is_stage0 {
                        // Keep the wide upstream trunk understated so the colored branches sing!
                        0x1b
                    } else {
                        0x32
                    };
                    window.paint_path(path, tint_bucket.with_alpha(alpha));
                }

                // Razor-sharp 1px top/bottom highlight edge for optical definition
                if lit || !dimmed {
                    let edge_a = if lit {
                        0xaa
                    } else if r.is_stage0 {
                        0x28
                    } else {
                        0x46
                    };
                    let mut top_e = PathBuilder::stroke(px(1.0));
                    top_e.move_to(point(px(r.x0), px(r.y0_top)));
                    top_e.cubic_bezier_to(
                        point(px(r.x1), px(r.y1_top)),
                        point(px(mid_x), px(r.y0_top)),
                        point(px(mid_x), px(r.y1_top)),
                    );
                    if let Ok(path) = top_e.build() {
                        window.paint_path(path, tint_bucket.with_alpha(edge_a));
                    }
                }
            }

            // Razor-thin 3px architectural node pins (crisp, never bulky!)
            for (x_pos, nodes) in [
                (x_col0, &col0_paint),
                (x_col1, &col1_paint),
                (x_col2, &col2_paint),
            ] {
                for n in nodes {
                    let active = sel_paint.is_none()
                        || sel_paint == Some(SankeyBucket::Total)
                        || sel_paint == Some(n.bucket)
                        || (n.bucket == SankeyBucket::Submitted
                            && sel_paint.is_some_and(|s| s != SankeyBucket::Saved));
                    if sel_paint == Some(n.bucket) {
                        let aura = Bounds::new(
                            point(px(x_pos - 2.5), px(oy + n.y - 1.5)),
                            gpui::size(px(pin_w + 5.0), px(n.h + 3.0)),
                        );
                        window.paint_quad(
                            gpui::fill(aura, n.bucket.with_alpha(0x38)).corner_radii(px(4.0)),
                        );
                    }
                    let bar = Bounds::new(
                        point(px(x_pos), px(oy + n.y)),
                        gpui::size(px(pin_w), px(n.h)),
                    );
                    let col = if active {
                        n.bucket.color()
                    } else {
                        n.bucket.with_alpha(0x38)
                    };
                    window.paint_quad(gpui::fill(bar, col).corner_radii(px(2.0)));
                }
            }
        },
    )
    .w_full()
    .h(px(diagram_h));

    // Exact per-ribbon mouse hit testing directly on the Sankey canvas!
    let hit_bounds_move = shared_bounds.clone();
    let col0_mv = col0.clone();
    let col1_mv = col1.clone();
    let col2_mv = col2.clone();
    let on_hov_canvas = on_hover.clone();

    let hit_bounds_click = shared_bounds;
    let col0_cl = col0.clone();
    let col1_cl = col1.clone();
    let col2_cl = col2.clone();
    let on_sel_canvas = on_select.clone();
    let on_zoom_canvas = on_zoom.clone();

    let hit_test = move |pos: gpui::Point<Pixels>,
                         bounds: Bounds<Pixels>,
                         c0: &[NodeGeom],
                         c1: &[NodeGeom],
                         c2: &[NodeGeom]|
          -> Option<SankeyBucket> {
        let mx: f32 = pos.x.into();
        let my: f32 = pos.y.into();
        let oy: f32 = bounds.origin.y.into();
        let (x0, x1, x2, ribbons) = build_ribbons(bounds, c0, c1, c2, zoom_root);
        // Check leaf/mid/root pins first (generous 12px horizontal grab zone)
        for (px_x, nodes) in [(x2, c2), (x1, c1), (x0, c0)] {
            if (mx - px_x).abs() <= 10.0 {
                for n in nodes {
                    if my >= oy + n.y - 4.0 && my <= oy + n.y + n.h + 4.0 {
                        return Some(n.bucket);
                    }
                }
            }
        }
        // Check curved ribbons directly!
        for r in ribbons.iter().rev() {
            if let Some((top, bot)) = r.y_span_at(mx)
                && my >= top - 2.0
                && my <= bot + 2.0
            {
                return Some(r.target);
            }
        }
        None
    };

    let stage_interactive = div()
        .id("sankey-interactive-stage")
        .relative()
        .flex_shrink_0()
        .w_full()
        .h(px(diagram_h + top_pad * 2.0))
        .pt(px(top_pad))
        .pb(px(top_pad))
        .cursor_pointer()
        .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, window, cx| {
            let hit = hit_bounds_move
                .borrow()
                .and_then(|b| hit_test(ev.position, b, &col0_mv, &col1_mv, &col2_mv));
            on_hov_canvas(hit, this, window, cx);
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                let hit = hit_bounds_click
                    .borrow()
                    .and_then(|b| hit_test(ev.position, b, &col0_cl, &col1_cl, &col2_cl));
                if let Some(bucket) = hit {
                    if bucket.has_children() && zoom_root != Some(bucket) {
                        on_zoom_canvas(Some(bucket), this, window, cx);
                    } else if bucket == SankeyBucket::Total {
                        on_zoom_canvas(None, this, window, cx);
                    } else {
                        let next = if selected == Some(bucket) {
                            None
                        } else {
                            Some(bucket)
                        };
                        on_sel_canvas(next, this, window, cx);
                    }
                } else if selected.is_some() {
                    on_sel_canvas(None, this, window, cx);
                }
            }),
        )
        .child(ribbon_canvas)
        // Column 0 root label cleanly to the left of the hairline pin
        .children(col0.iter().map(|n| {
            let on_zm = on_zoom.clone();
            div()
                .id("sk-n0-root")
                .absolute()
                .left(px(4.0))
                .w(px(92.0))
                .top(px(top_pad + n.y + (n.h * 0.5) - 18.0))
                .cursor_pointer()
                .flex()
                .flex_col()
                .items_end()
                .on_click(cx.listener(move |this, _, window, cx| {
                    on_zm(None, this, window, cx);
                }))
                .child(
                    div()
                        .text_size(px(19.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text_primary())
                        .child(n.count.to_string()),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme::text_muted())
                        .child(n.bucket.label()),
                )
        }))
        // Column 1 labels anchored cleanly ABOVE Submitted pin and BELOW Saved pin (zero ribbon collision!)
        .children(col1.iter().map(|n| {
            let b = n.bucket;
            let is_sel = selected == Some(b);
            let is_hl = focus_bucket == Some(b);
            let on_sel = on_select.clone();
            let on_zm = on_zoom.clone();
            let on_hov = on_hover.clone();
            let y_pos = if b == SankeyBucket::Submitted {
                (top_pad + n.y - 24.0).max(4.0)
            } else {
                top_pad + n.y + n.h + 6.0
            };
            div()
                .id(SharedString::from(format!("sk-n1-{}", b.label())))
                .absolute()
                .left(gpui::relative(0.42))
                .ml(px(-28.0))
                .top(px(y_pos))
                .px(px(6.0))
                .py(px(1.5))
                .rounded(px(5.0))
                .when(is_sel, |d| d.bg(theme::surface_subtle()))
                .cursor_pointer()
                .flex()
                .items_center()
                .gap(px(5.0))
                .on_hover(cx.listener(move |this, h: &bool, window, cx| {
                    on_hov(if *h { Some(b) } else { None }, this, window, cx);
                }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    if b.has_children() {
                        on_zm(Some(b), this, window, cx);
                    } else {
                        let next = if is_sel { None } else { Some(b) };
                        on_sel(next, this, window, cx);
                    }
                }))
                .child(
                    div()
                        .text_size(px(11.5))
                        .font_weight(if is_hl {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::MEDIUM
                        })
                        .text_color(if is_hl {
                            b.color()
                        } else {
                            theme::text_secondary()
                        })
                        .child(b.label()),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme::text_muted())
                        .child(n.count.to_string()),
                )
        }))
        // Column 2 labels cleanly to the right of the hairline pins
        .children(col2.iter().map(|n| {
            let b = n.bucket;
            let is_sel = selected == Some(b);
            let is_hl = focus_bucket == Some(b);
            let on_sel = on_select.clone();
            let on_zm = on_zoom.clone();
            let on_hov = on_hover.clone();
            let pct = ((n.count as f32 / denom as f32) * 100.0).round() as usize;
            div()
                .id(SharedString::from(format!("sk-n2-{}", b.label())))
                .absolute()
                .right(px(8.0))
                .w(px(166.0))
                .top(px(top_pad + n.y))
                .h(px(n.h))
                .px(px(8.0))
                .rounded(px(6.0))
                .when(is_sel, |d| d.bg(theme::surface_subtle()))
                .cursor_pointer()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .on_hover(cx.listener(move |this, h: &bool, window, cx| {
                    on_hov(if *h { Some(b) } else { None }, this, window, cx);
                }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    if b.has_children() && zoom_root != Some(b) {
                        on_zm(Some(b), this, window, cx);
                    } else {
                        let next = if is_sel { None } else { Some(b) };
                        on_sel(next, this, window, cx);
                    }
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(if is_hl {
                                    FontWeight::SEMIBOLD
                                } else {
                                    FontWeight::MEDIUM
                                })
                                .text_color(if is_hl {
                                    b.color()
                                } else {
                                    theme::text_primary()
                                })
                                .child(b.label()),
                        )
                        .when(b.has_children() && zoom_root != Some(b), |r| {
                            r.child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(theme::text_muted())
                                    .child("›"),
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if is_hl {
                                    theme::text_primary()
                                } else {
                                    theme::text_secondary()
                                })
                                .child(n.count.to_string()),
                        )
                        .child(
                            div()
                                .text_size(px(10.5))
                                .text_color(theme::text_muted())
                                .child(format!("{pct}%")),
                        ),
                )
        }));

    let animated_stage = if reduce_motion {
        stage_interactive.into_any_element()
    } else {
        stage_interactive
            .with_animation(
                SharedString::from(format!("sankey-enter-{anim_epoch}")),
                Animation::new(Duration::from_millis(260)).with_easing(ease_out_quint()),
                |el, t| el.opacity(t).mt(px(10.0 * (1.0 - t))),
            )
            .into_any_element()
    };

    let drill_cards: Vec<RoleCard> = selected
        .map(|b| {
            cards
                .iter()
                .filter(|c| matches_bucket(c, b))
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    div()
        .id("sankey-flow-view")
        .relative()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .overflow_hidden()
        .flex()
        .flex_col()
        .justify_center()
        // Clean breadcrumb bar (only appears when zoomed into a parent branch!)
        .when_some(zoom_root, |d, zr| {
            let on_back = on_zoom.clone();
            d.child(
                div()
                    .absolute()
                    .top_1()
                    .left_1()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_xs()
                    .child(
                        div()
                            .id("sk-crumb-root")
                            .px(px(6.0))
                            .py(px(2.0))
                            .rounded(px(5.0))
                            .hover(|s| s.bg(theme::row_hover()).text_color(theme::text_primary()))
                            .cursor_pointer()
                            .text_color(theme::text_muted())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                on_back(None, this, window, cx);
                            }))
                            .child("← All roles"),
                    )
                    .child(div().text_color(theme::text_muted()).child("›"))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(zr.color())
                            .child(zr.label()),
                    ),
            )
        })
        .child(animated_stage)
        // Floating scrollable drill-down popover (keeps full Sankey calm behind it!)
        .when_some(selected, |d, active_bucket| {
            let on_close = on_select.clone();
            let pct = if submitted > 0
                && !matches!(active_bucket, SankeyBucket::Total | SankeyBucket::Saved)
            {
                Some(((drill_cards.len() as f32 / submitted as f32) * 100.0).round() as usize)
            } else {
                None
            };
            let card = div()
                .id("sankey-drill-popover")
                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .w(px(360.0))
                .max_h(px(340.0))
                .rounded(px(12.0))
                .bg(theme::popover_glass())
                .border_1()
                .border_color(theme::hairline_strong())
                .shadow_lg()
                .overflow_hidden()
                .flex()
                .flex_col()
                .child(
                    div()
                        .px_4()
                        .py_3()
                        .border_b_1()
                        .border_color(theme::hairline())
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(7.0))
                                        .h(px(7.0))
                                        .rounded(px(99.0))
                                        .bg(active_bucket.color()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme::text_primary())
                                        .child(active_bucket.label()),
                                )
                                .child(div().text_xs().text_color(theme::text_muted()).child(
                                    match pct {
                                        Some(p) => format!("{} · {p}%", drill_cards.len()),
                                        None => drill_cards.len().to_string(),
                                    },
                                )),
                        )
                        .child(
                            div()
                                .id("sk-drill-close")
                                .px(px(6.0))
                                .py(px(2.0))
                                .rounded(px(5.0))
                                .hover(|s| {
                                    s.bg(theme::row_hover()).text_color(theme::text_primary())
                                })
                                .cursor_pointer()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    on_close(None, this, window, cx);
                                }))
                                .child("← Back"),
                        ),
                )
                .child(
                    div()
                        .id("sankey-drill-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .p_2()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .children(drill_cards.into_iter().enumerate().map(|(i, role)| {
                            let open_cb = on_open_role.clone();
                            let role_click = role.clone();
                            div()
                                .id(SharedString::from(format!("sk-pop-role-{i}")))
                                .w_full()
                                .px_3()
                                .py_2()
                                .rounded(px(7.0))
                                .hover(|s| s.bg(theme::row_hover()))
                                .cursor_pointer()
                                .overflow_hidden()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    open_cb(role_click.clone(), this, window, cx);
                                }))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .text_xs()
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(theme::text_primary())
                                                .child(role.company.clone()),
                                        )
                                        .child(
                                            div()
                                                .flex_shrink_0()
                                                .text_size(px(10.5))
                                                .text_color(theme::text_muted())
                                                .child(
                                                    role.note_line
                                                        .clone()
                                                        .unwrap_or_else(|| role.checked.clone()),
                                                ),
                                        ),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_size(px(11.0))
                                        .text_color(theme::text_muted())
                                        .child(role.title.clone()),
                                )
                        })),
                );

            let pop = if reduce_motion {
                card.into_any_element()
            } else {
                card.with_animation(
                    SharedString::from(format!("sk-pop-{}", active_bucket.label())),
                    Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint()),
                    |el, t| el.opacity(t).mt(px(8.0 * (1.0 - t))),
                )
                .into_any_element()
            };

            d.child(div().absolute().right_4().bottom_4().child(pop))
        })
        .into_any_element()
}
