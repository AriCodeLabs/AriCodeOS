// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Resizing windows by their edges and corners (ADR 0085): where the owner can grab
//! a window, the size a drag asks for, the cursor that shows it, and the exchange
//! with the window's program, which paints each new size before it is shown.

use crate::{corner_inset, edge, Rect, RADIUS, TITLE_H};

/// Where a window that can be resized is grabbed, and how small a drag makes it.
/// A policy, not a law: the compositor holds one value and may change it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grips {
    /// The band outside the frame that grabs its side. The frame has no border to
    /// grab, so the band lies over the shadow, as in GNOME (mutter's
    /// `draggable-border-width`, 10 px) and Windows 10 (8 px of invisible border).
    pub band: i32,
    /// How far from a corner, along either side, a grab takes both sides: a corner
    /// is a square of `band` otherwise, too small to find with a touchpad, and the
    /// rounded corner leaves the visible window `RADIUS` short of it.
    pub corner: i32,
    /// The smallest content a drag asks for: room for a line of text and a button,
    /// and for the title bar's three circles.
    pub min_content: (u32, u32),
}

impl Grips {
    /// 8 px of band, as Windows; corners reach the radius and the band beyond it.
    pub const DEFAULT: Grips = Grips {
        band: 8,
        corner: RADIUS + 6,
        min_content: (160, 80),
    };

    /// Which edges of a resizable window's `frame` a point grabs: in the band
    /// outside it, or in the notch a rounded corner leaves inside it, where nothing
    /// of the window shows. Near a corner, both of its sides. 0 elsewhere.
    pub fn edges_at(&self, frame: &Rect, x: i32, y: i32) -> u8 {
        let band = self.band;
        let wide = Rect::new(
            frame.x - band,
            frame.y - band,
            frame.w + 2 * band as u32,
            frame.h + 2 * band as u32,
        );
        if !wide.contains(x, y) {
            return 0;
        }
        if frame.contains(x, y) {
            return notch_edges(frame, x, y);
        }
        let mut edges = 0;
        if x < frame.x {
            edges |= edge::LEFT;
        }
        if x >= frame.right() {
            edges |= edge::RIGHT;
        }
        if y < frame.y {
            edges |= edge::TOP;
        }
        if y >= frame.bottom() {
            edges |= edge::BOTTOM;
        }
        // Along a side, near a corner: that corner. Half the side at most, so a
        // small window still has both.
        let near = |at: i32, low: i32, len: u32| {
            let reach = self.corner.min(len as i32 / 2);
            (at < low + reach, at >= low + len as i32 - reach)
        };
        if edges & (edge::LEFT | edge::RIGHT) == 0 {
            let (left, right) = near(x, frame.x, frame.w);
            edges |= if left { edge::LEFT } else { 0 } | if right { edge::RIGHT } else { 0 };
        }
        if edges & (edge::TOP | edge::BOTTOM) == 0 {
            let (top, bottom) = near(y, frame.y, frame.h);
            edges |= if top { edge::TOP } else { 0 } | if bottom { edge::BOTTOM } else { 0 };
        }
        edges
    }

    /// The content a drag of `(dx, dy)` on the `edges` of `start` asks for. The side
    /// opposite the one dragged stays; the content keeps `min_content`; the frame stays
    /// inside `bounds` (the screen under the bar) on the sides dragged, and is never
    /// larger than `bounds`.
    pub fn resized(&self, start: Rect, edges: u8, (dx, dy): (i32, i32), bounds: &Rect) -> Rect {
        let title = TITLE_H as i32;
        let (x, right) = drag_span(
            (start.x, start.right()),
            (edges & edge::LEFT != 0, edges & edge::RIGHT != 0),
            dx,
            self.min_content.0 as i32,
            (bounds.x, bounds.right()),
        );
        // Vertically the frame counts: its title bar must stay below the bar.
        let (top, bottom) = drag_span(
            (start.y - title, start.bottom()),
            (edges & edge::TOP != 0, edges & edge::BOTTOM != 0),
            dy,
            self.min_content.1 as i32 + title,
            (bounds.y, bounds.bottom()),
        );
        Rect::new(
            x,
            top + title,
            (right - x) as u32,
            (bottom - top - title) as u32,
        )
    }
}

/// One axis of a drag: `span` with its low or high end moved by `d`, at least `min`
/// long, the moved end inside `bounds`, and never longer than `bounds` unless `min`
/// is.
fn drag_span(
    (lo, hi): (i32, i32),
    (low, high): (bool, bool),
    d: i32,
    min: i32,
    (floor, ceil): (i32, i32),
) -> (i32, i32) {
    let room = (ceil - floor).max(min);
    let (mut a, mut b) = (lo, hi);
    if high {
        b = (hi + d).min(ceil).max(a + min).min(a + room);
    }
    if low {
        a = (lo + d).max(floor).min(b - min).max(b - room);
    }
    (a, b)
}

/// The corner whose notch, inside `frame` but outside its rounded shape, holds the
/// point: both its sides, or 0.
fn notch_edges(frame: &Rect, x: i32, y: i32) -> u8 {
    let r = RADIUS.min(frame.w as i32 / 2).min(frame.h as i32 / 2);
    let (from_left, from_right) = (x - frame.x, frame.right() - 1 - x);
    let (from_top, from_bottom) = (y - frame.y, frame.bottom() - 1 - y);
    let (side, horizontal) = if from_left < r {
        (from_left, edge::LEFT)
    } else if from_right < r {
        (from_right, edge::RIGHT)
    } else {
        return 0;
    };
    let (row, vertical) = if from_top < r {
        (from_top, edge::TOP)
    } else if from_bottom < r {
        (from_bottom, edge::BOTTOM)
    } else {
        return 0;
    };
    if side < corner_inset(row as usize) {
        horizontal | vertical
    } else {
        0
    }
}

/// The pointer's picture: an arrow, or two arrows along the sides a grab moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Cursor {
    #[default]
    Arrow,
    /// The left or right side.
    LeftRight,
    /// The top or bottom side.
    UpDown,
    /// The top-left or bottom-right corner.
    Falling,
    /// The top-right or bottom-left corner.
    Rising,
}

/// The side of the square a resize cursor is drawn in; the point is its centre.
pub const RESIZE_CURSOR: u32 = 15;
const HALF: i32 = RESIZE_CURSOR as i32 / 2;

impl Cursor {
    /// The cursor that says what a grab of these `edge`s does.
    pub fn for_edges(edges: u8) -> Self {
        let horizontal = edges & (edge::LEFT | edge::RIGHT);
        let vertical = edges & (edge::TOP | edge::BOTTOM);
        match (horizontal, vertical) {
            (0, 0) => Cursor::Arrow,
            (_, 0) => Cursor::LeftRight,
            (0, _) => Cursor::UpDown,
            (h, v) if (h == edge::LEFT) == (v == edge::TOP) => Cursor::Falling,
            _ => Cursor::Rising,
        }
    }

    /// A word for the frame trace (`gui_trace`), which tests read.
    pub fn name(self) -> &'static str {
        match self {
            Cursor::Arrow => "arrow",
            Cursor::LeftRight => "left-right",
            Cursor::UpDown => "up-down",
            Cursor::Falling => "nw-se",
            Cursor::Rising => "ne-sw",
        }
    }

    /// The pixels this cursor may paint with its point at `(x, y)`.
    pub const fn rect(self, x: i32, y: i32) -> Rect {
        match self {
            Cursor::Arrow => Rect::new(x, y, crate::CURSOR_W, crate::CURSOR_H),
            _ => Rect::new(x - HALF, y - HALF, RESIZE_CURSOR, RESIZE_CURSOR),
        }
    }

    /// Whether `(dx, dy)` from the point is white in a resize cursor: a 1 px shaft
    /// with a head at each end. Black is what touches white and is not; the rest
    /// shows what is under it.
    pub(crate) fn fills(self, dx: i32, dy: i32) -> bool {
        // Each shape is the left-right one turned: along the arrows, and across.
        let (along, across) = match self {
            Cursor::Arrow => return false,
            Cursor::LeftRight => (dx, dy),
            Cursor::UpDown => (dy, dx),
            Cursor::Falling => return diagonal(dx, dy),
            Cursor::Rising => return diagonal(-dx, dy),
        };
        let tip = HALF - 1;
        let from_tip = tip - along.abs();
        let shaft = across == 0 && along.abs() <= tip;
        let head = (0..=3).contains(&from_tip) && across.abs() <= from_tip;
        shaft || head
    }
}

/// The top-left to bottom-right double arrow: a shaft along the diagonal, thick
/// enough to meet the heads without a gap, and at each end a right triangle whose
/// legs lie along the square's sides.
fn diagonal(dx: i32, dy: i32) -> bool {
    let tip = HALF - 1;
    let shaft = (dx - dy).abs() <= 1 && (dx + dy).abs() <= 8;
    let head = |x: i32, y: i32| x >= 0 && y >= 0 && x + y <= 6;
    shaft || head(dx + tip, dy + tip) || head(tip - dx, tip - dy)
}

/// How long the compositor waits for a program's answer to one size before it asks
/// for the latest: a program on the reference laptop repaints a window in tens of
/// milliseconds, so one that takes longer has fallen behind or stopped.
pub const PATIENCE_MS: u64 = 250;

/// The sizes asked and not yet answered that are kept: an answer to one of them
/// takes the window to the place asked with it.
const KEPT: usize = 4;

/// What to do now about a place the owner wants for a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Step {
    /// Move the window's content here at once: its size does not change.
    pub move_to: Option<(i32, i32)>,
    /// Ask its program for this size (`Configure`).
    pub ask: Option<(u32, u32)>,
}

/// The exchange with a program about its window's size (ADR 0085): the owner wants
/// a place, the program is asked for its size, and the window takes that place only
/// when the program answers with a buffer of that size, so it is never shown cut,
/// stretched or half painted. One request at a time: while one waits, the latest
/// place the owner wants waits behind it, and a drag sends no more than the program
/// can follow. A program that does not answer is asked again after `PATIENCE_MS`,
/// for the latest only; it blocks nothing and gets nothing more.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Placing {
    /// Places asked, oldest first, not yet answered.
    sent: [Option<Rect>; KEPT],
    /// When the newest of them was asked (ms since boot).
    sent_ms: u64,
    /// The place the owner wants that has not been asked yet.
    wanted: Option<Rect>,
}

impl Placing {
    /// No size is asked or waiting.
    pub fn is_idle(&self) -> bool {
        self.sent[0].is_none() && self.wanted.is_none()
    }

    fn newest(&self) -> Option<Rect> {
        self.sent.iter().flatten().last().copied()
    }

    /// The owner wants the window, now at `current`, at `to`. A window that cannot
    /// be resized only moves, and only when the size stays.
    pub fn want(&mut self, current: Rect, to: Rect, resizable: bool, now_ms: u64) -> Step {
        let same = (current.w, current.h) == (to.w, to.h);
        if same && self.is_idle() {
            return Step {
                move_to: Some((to.x, to.y)),
                ask: None,
            };
        }
        if !resizable || self.wanted.or(self.newest()) == Some(to) {
            return Step::default();
        }
        let mut step = Step::default();
        if same {
            // Back to its size while another is asked: no answer to that one may
            // move it now, and its program is asked for this size again.
            self.sent = [None; KEPT];
            step.move_to = Some((to.x, to.y));
        }
        self.wanted = Some(to);
        step.ask = self.send(now_ms);
        step
    }

    /// The program answered with a buffer of `width` x `height`: where the window
    /// goes with it, if anywhere (an answer to a size not asked leaves it where it
    /// is), whether it was asked, and the next size to ask, if one waits.
    pub fn answered(&mut self, width: u32, height: u32, now_ms: u64) -> Answer {
        let mut answer = Answer::default();
        let found = (0..KEPT)
            .rev()
            .find(|&i| self.sent[i].is_some_and(|r| (r.w, r.h) == (width, height)));
        if let Some(i) = found {
            let place = self.sent[i];
            answer.asked = true;
            answer.move_to = place.map(|r| (r.x, r.y));
            // What was asked before it will not come any more.
            self.sent.copy_within(i + 1.., 0);
            self.sent[KEPT - 1 - i..].fill(None);
        }
        // A size the owner went back to while it was asked: its place is the newer.
        if let Some(w) = self.wanted.filter(|w| (w.w, w.h) == (width, height)) {
            self.wanted = None;
            answer.asked = true;
            answer.move_to = Some((w.x, w.y));
        }
        if self.sent[0].is_none() {
            answer.ask = self.send(now_ms);
        }
        answer
    }

    /// A size to ask now because the last one went unanswered for `PATIENCE_MS`.
    pub fn due(&mut self, now_ms: u64) -> Option<(u32, u32)> {
        self.send(now_ms)
    }

    /// When [`due`](Self::due) next has something to ask.
    pub fn deadline(&self) -> Option<u64> {
        self.wanted
            .map(|_| self.sent_ms.saturating_add(PATIENCE_MS))
    }

    /// Asks for the wanted place, if the program has answered the last request or
    /// was given `PATIENCE_MS` for it.
    fn send(&mut self, now_ms: u64) -> Option<(u32, u32)> {
        let to = self.wanted?;
        if self.sent[0].is_some() && now_ms < self.sent_ms.saturating_add(PATIENCE_MS) {
            return None;
        }
        self.wanted = None;
        if self.sent[KEPT - 1].is_some() {
            self.sent.copy_within(1.., 0);
            self.sent[KEPT - 1] = None;
        }
        let free = self
            .sent
            .iter()
            .position(Option::is_none)
            .unwrap_or(KEPT - 1);
        self.sent[free] = Some(to);
        self.sent_ms = now_ms;
        Some((to.w, to.h))
    }
}

/// What a program's new size means: see [`Placing::answered`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Answer {
    pub move_to: Option<(i32, i32)>,
    /// It was a size the compositor asked for.
    pub asked: bool,
    pub ask: Option<(u32, u32)>,
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    const G: Grips = Grips::DEFAULT;

    #[test]
    fn the_band_grabs_sides_and_corners_reach_along_them() {
        let f = Rect::new(100, 100, 400, 300);
        let (r, b) = (f.right(), f.bottom());
        assert_eq!(G.edges_at(&f, r, 250), edge::RIGHT);
        assert_eq!(G.edges_at(&f, r + G.band - 1, 250), edge::RIGHT);
        assert_eq!(G.edges_at(&f, r + G.band, 250), 0, "past the band");
        assert_eq!(G.edges_at(&f, 99, 250), edge::LEFT);
        assert_eq!(G.edges_at(&f, 300, 99), edge::TOP);
        assert_eq!(G.edges_at(&f, 300, b), edge::BOTTOM);
        // The square beyond the corner, and along each side up to `corner`.
        assert_eq!(G.edges_at(&f, r + 2, b + 2), edge::RIGHT | edge::BOTTOM);
        let near = r - G.corner;
        assert_eq!(G.edges_at(&f, near, b + 2), edge::RIGHT | edge::BOTTOM);
        assert_eq!(G.edges_at(&f, near - 1, b + 2), edge::BOTTOM);
        assert_eq!(
            G.edges_at(&f, r + 2, b - G.corner),
            edge::RIGHT | edge::BOTTOM
        );
        assert_eq!(G.edges_at(&f, r + 2, b - G.corner - 1), edge::RIGHT);
        assert_eq!(
            G.edges_at(&f, 100 + G.corner - 1, 98),
            edge::LEFT | edge::TOP
        );
        assert_eq!(G.edges_at(&f, 98, 399), edge::LEFT | edge::BOTTOM);
        assert_eq!(G.edges_at(&f, r + 1, 100), edge::RIGHT | edge::TOP);
    }

    #[test]
    fn inside_the_frame_only_the_corners_notches_grab() {
        let f = Rect::new(100, 100, 400, 300);
        let (r, b) = (f.right() - 1, f.bottom() - 1);
        for (x, y, edges) in [
            (100, 100, edge::LEFT | edge::TOP),
            (r, 100, edge::RIGHT | edge::TOP),
            (100, b, edge::LEFT | edge::BOTTOM),
            (r, b, edge::RIGHT | edge::BOTTOM),
        ] {
            assert_eq!(G.edges_at(&f, x, y), edges, "({x},{y})");
        }
        // Where the window shows, it is the window's: the corner's curve, its sides.
        assert_eq!(G.edges_at(&f, 100 + RADIUS, 100), 0);
        assert_eq!(G.edges_at(&f, 100, 100 + RADIUS), 0);
        assert_eq!(G.edges_at(&f, r - RADIUS, b), 0);
        assert_eq!(G.edges_at(&f, 300, 250), 0);
        // The notch is what the corner's drawing leaves out.
        for row in 0..RADIUS {
            let inset = corner_inset(row as usize);
            assert_ne!(G.edges_at(&f, r - inset + 1, b - row), 0, "row {row}");
            assert_eq!(G.edges_at(&f, r - inset, b - row), 0, "row {row}");
        }
    }

    #[test]
    fn a_small_window_keeps_two_corners_on_each_side() {
        let f = Rect::new(100, 100, 20, 12);
        assert_eq!(G.edges_at(&f, 109, 98), edge::LEFT | edge::TOP);
        assert_eq!(G.edges_at(&f, 110, 98), edge::RIGHT | edge::TOP);
        assert_eq!(G.edges_at(&f, 98, 106), edge::LEFT | edge::BOTTOM);
    }

    #[test]
    fn a_drag_keeps_the_far_side_the_minimum_and_the_screen() {
        let screen = Rect::new(0, 24, 1280, 776);
        let start = Rect::new(100, 100, 400, 300);
        let rb = edge::RIGHT | edge::BOTTOM;
        assert_eq!(
            G.resized(start, rb, (50, -20), &screen),
            Rect::new(100, 100, 450, 280)
        );
        assert_eq!(
            G.resized(start, edge::LEFT, (30, 0), &screen),
            Rect::new(130, 100, 370, 300),
            "the right side stays"
        );
        let r = G.resized(start, edge::TOP, (0, 1000), &screen);
        assert_eq!((r.h, r.bottom()), (G.min_content.1, start.bottom()));
        let r = G.resized(start, edge::RIGHT, (-1000, 0), &screen);
        assert_eq!((r.x, r.w), (100, G.min_content.0));
        // Past the screen: the dragged side stops at its edge.
        let r = G.resized(start, edge::LEFT, (-500, 0), &screen);
        assert_eq!((r.x, r.right()), (0, start.right()));
        let r = G.resized(start, rb, (5000, 5000), &screen);
        assert_eq!((r.right(), r.bottom()), (1280, 800));
        // The title bar stays under the bar: the content's top is the bar and the
        // title below it, and the bottom stays.
        let r = G.resized(start, edge::TOP, (0, -500), &screen);
        assert_eq!((r.y, r.bottom()), (24 + TITLE_H as i32, start.bottom()));
    }

    #[test]
    fn a_window_partly_off_the_screen_grows_no_larger_than_it() {
        let screen = Rect::new(0, 24, 800, 576);
        // Its right side is far off the screen; its left is dragged to the edge.
        let start = Rect::new(500, 200, 700, 200);
        let r = G.resized(start, edge::LEFT, (-500, 0), &screen);
        assert_eq!((r.w, r.right()), (800, start.right()));
        // And a minimum larger than the screen still holds.
        let tiny = Rect::new(0, 24, 100, 60);
        let r = G.resized(start, edge::LEFT, (600, 0), &tiny);
        assert_eq!(r.w, G.min_content.0);
    }

    #[test]
    fn each_grab_has_its_cursor() {
        use edge::*;
        assert_eq!(Cursor::for_edges(0), Cursor::Arrow);
        assert_eq!(Cursor::for_edges(LEFT), Cursor::LeftRight);
        assert_eq!(Cursor::for_edges(RIGHT), Cursor::LeftRight);
        assert_eq!(Cursor::for_edges(TOP), Cursor::UpDown);
        assert_eq!(Cursor::for_edges(BOTTOM), Cursor::UpDown);
        assert_eq!(Cursor::for_edges(LEFT | TOP), Cursor::Falling);
        assert_eq!(Cursor::for_edges(RIGHT | BOTTOM), Cursor::Falling);
        assert_eq!(Cursor::for_edges(RIGHT | TOP), Cursor::Rising);
        assert_eq!(Cursor::for_edges(LEFT | BOTTOM), Cursor::Rising);
    }

    #[test]
    fn resize_cursors_are_symmetric_inside_their_square_and_white_at_the_point() {
        for c in [
            Cursor::LeftRight,
            Cursor::UpDown,
            Cursor::Falling,
            Cursor::Rising,
        ] {
            assert!(c.fills(0, 0), "{c:?}: the point shows");
            let mut white = 0;
            for dy in -HALF - 2..=HALF + 2 {
                for dx in -HALF - 2..=HALF + 2 {
                    let on = c.fills(dx, dy);
                    assert_eq!(on, c.fills(-dx, -dy), "{c:?} ({dx},{dy})");
                    // White and its black outline stay inside the square.
                    if on {
                        white += 1;
                        assert!(dx.abs() < HALF && dy.abs() < HALF, "{c:?} ({dx},{dy})");
                    }
                }
            }
            assert!(white > 20, "{c:?} has a shape");
        }
        // The turned ones are the same picture.
        let count = |c: Cursor| {
            let all: Vec<(i32, i32)> = (-HALF..=HALF)
                .flat_map(|y| (-HALF..=HALF).map(move |x| (x, y)))
                .collect();
            all.iter().filter(|&&(x, y)| c.fills(x, y)).count()
        };
        assert_eq!(count(Cursor::LeftRight), count(Cursor::UpDown));
        assert_eq!(count(Cursor::Falling), count(Cursor::Rising));
    }

    fn at(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect::new(x, y, w, h)
    }

    #[test]
    fn a_window_that_keeps_its_size_moves_at_once() {
        let mut p = Placing::default();
        let now = at(100, 100, 400, 300);
        let step = p.want(now, at(150, 120, 400, 300), false, 0);
        assert_eq!(step.move_to, Some((150, 120)));
        assert_eq!(step.ask, None);
        assert!(p.is_idle());
        // A new size for a window that cannot take one: nothing at all.
        assert_eq!(
            p.want(now, at(100, 100, 500, 300), false, 0),
            Step::default()
        );
        assert!(p.is_idle());
    }

    #[test]
    fn one_size_is_asked_at_a_time_and_the_latest_follows_the_answer() {
        let mut p = Placing::default();
        let now = at(100, 100, 400, 300);
        let first = p.want(now, at(100, 100, 410, 300), true, 0);
        assert_eq!(
            first,
            Step {
                move_to: None,
                ask: Some((410, 300))
            }
        );
        // A drag goes on: these wait behind the first, and only the last counts.
        for (i, w) in (420..=500).step_by(10).enumerate() {
            let step = p.want(now, at(100, 100, w, 300), true, 1 + i as u64);
            assert_eq!(step, Step::default(), "{w}");
        }
        assert_eq!(p.deadline(), Some(PATIENCE_MS));
        let a = p.answered(410, 300, 20);
        assert_eq!(
            a,
            Answer {
                move_to: Some((100, 100)),
                asked: true,
                ask: Some((500, 300))
            }
        );
        let a = p.answered(500, 300, 40);
        assert_eq!(
            a,
            Answer {
                move_to: Some((100, 100)),
                asked: true,
                ask: None
            }
        );
        assert!(p.is_idle());
        assert_eq!(p.deadline(), None);
    }

    #[test]
    fn a_left_edge_answer_takes_the_place_asked_with_it() {
        let mut p = Placing::default();
        let now = at(100, 100, 400, 300);
        p.want(now, at(90, 100, 410, 300), true, 0);
        p.want(now, at(60, 100, 440, 300), true, 5);
        let a = p.answered(410, 300, 10);
        assert_eq!((a.move_to, a.ask), (Some((90, 100)), Some((440, 300))));
        let a = p.answered(440, 300, 30);
        assert_eq!(a.move_to, Some((60, 100)), "never a right edge that jumps");
    }

    #[test]
    fn a_program_that_does_not_answer_is_asked_again_once_then_left_alone() {
        let mut p = Placing::default();
        let now = at(100, 100, 400, 300);
        assert!(p.want(now, at(100, 100, 410, 300), true, 0).ask.is_some());
        let mut asked = 1;
        for t in 1..2000u64 {
            let w = 410 + t as u32 / 10;
            asked += p.want(now, at(100, 100, w, 300), true, t).ask.is_some() as u32;
            asked += p.due(t).is_some() as u32;
        }
        assert_eq!(asked, (2000 / PATIENCE_MS) as u32, "once each patience");
        // The drag ended: the last place is asked once more at most, then nothing.
        let mut later = 0;
        for t in 2000..10_000u64 {
            later += p.due(t).is_some() as u32;
        }
        assert!(later <= 1, "{later}");
        assert_eq!(p.deadline(), None);
        // And it can still be moved where its size stays.
        let step = p.want(now, at(300, 300, 400, 300), true, 10_000);
        assert_eq!(step.move_to, Some((300, 300)));
    }

    #[test]
    fn a_slow_answer_to_an_older_size_still_finds_its_place() {
        let mut p = Placing::default();
        let now = at(100, 100, 400, 300);
        p.want(now, at(90, 100, 410, 300), true, 0);
        p.want(now, at(80, 100, 420, 300), true, 1);
        assert_eq!(p.due(PATIENCE_MS), Some((420, 300)));
        let a = p.answered(410, 300, PATIENCE_MS + 1);
        assert_eq!(a.move_to, Some((90, 100)));
        let a = p.answered(420, 300, PATIENCE_MS + 2);
        assert_eq!(a.move_to, Some((80, 100)));
        // More asked than kept: the oldest is forgotten, and its answer moves nothing.
        let mut p = Placing::default();
        for i in 0..=KEPT as u32 {
            let w = 410 + 10 * i;
            p.want(
                now,
                at(100 - 10 * i as i32, 100, w, 300),
                true,
                i as u64 * PATIENCE_MS,
            );
        }
        assert_eq!(p.answered(410, 300, 10_000), Answer::default());
        assert!(p.answered(450, 300, 10_001).asked);
    }

    #[test]
    fn going_back_to_the_size_shown_asks_for_it_and_drops_older_answers() {
        let mut p = Placing::default();
        let now = at(100, 100, 400, 300);
        p.want(now, at(100, 100, 440, 300), true, 0);
        let back = p.want(now, at(120, 100, 400, 300), true, 5);
        assert_eq!(back.move_to, Some((120, 100)), "it moves at once");
        assert_eq!(
            back.ask,
            Some((400, 300)),
            "and the program is asked for it"
        );
        // The answer to the size abandoned is not one asked any more.
        let a = p.answered(440, 300, 10);
        assert_eq!((a.move_to, a.asked), (None, false));
        let a = p.answered(400, 300, 20);
        assert_eq!(a.move_to, Some((120, 100)));
        assert!(p.is_idle());
    }

    #[test]
    fn a_size_the_program_chose_moves_nothing() {
        let mut p = Placing::default();
        assert_eq!(p.answered(500, 350, 0), Answer::default());
        assert!(p.is_idle());
    }
}
