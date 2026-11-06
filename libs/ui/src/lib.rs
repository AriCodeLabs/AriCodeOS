// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The desktop's toolkit (ADR 0078, stage 2): retained widgets in stacks, keyboard
//! focus, and only what changed painted again.
//!
//! A program builds its [`Ui`] once, gives it the window's events with
//! [`handle`](Ui::handle), reads back what the owner did with
//! [`next_action`](Ui::next_action), changes what it shows through the setters, and
//! paints with [`paint`](Ui::paint), which answers the rectangles to present.
//!
//! Each change marks its area, and painting walks the whole tree clipped to the marked
//! areas. No widget has a second way of drawing itself, so a partial paint is the full
//! paint's pixels by construction, and a host test holds it to that. Sizes and colours
//! are the theme's: a widget names a role, never a value.

#![no_std]

extern crate alloc;

pub mod edit;
pub mod theme;

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use aricode_gfx::{Canvas, Damage, Rect};
use aricode_input::term::{Key, KeyDecoder};
use aricode_proto::window::{mods, DamageRect, Event};
use aricode_text::Typesetter;
use edit::TextEdit;
pub use theme::{Fill, Space, Style, Theme, Tone, Type};

/// The window system's left button.
const LEFT: u8 = 1;

/// A widget of a [`Ui`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Id(u16);

/// What the owner did, in the order it happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// A button: clicked, or Enter or Space while focused.
    Pressed(Id),
    /// A list's selection moved to this row.
    Selected(Id, usize),
    /// A list's row was opened: Enter, or a click on the selected row.
    Activated(Id, usize),
    /// A field's text changed.
    Edited(Id),
    /// Enter in a field.
    Submitted(Id),
    /// The Delete key on a list's row.
    DeleteRow(Id, usize),
    /// Escape.
    Cancel,
    /// Ctrl and a letter that no widget took.
    Shortcut(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Column,
    Row,
}

/// Where children go across a stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Stretch,
    Start,
    Center,
    End,
}

/// How a stack lays out its children.
#[derive(Debug, Clone, Copy)]
pub struct Stack {
    pub axis: Axis,
    pub gap: Space,
    pub pad: Space,
    pub align: Align,
    pub fill: Fill,
    pub card: bool,
}

impl Stack {
    pub const fn column() -> Self {
        Self {
            axis: Axis::Column,
            gap: Space::Control,
            pad: Space::None,
            align: Align::Stretch,
            fill: Fill::None,
            card: false,
        }
    }

    pub const fn row() -> Self {
        Self {
            axis: Axis::Row,
            align: Align::Center,
            ..Self::column()
        }
    }

    pub const fn gap(mut self, gap: Space) -> Self {
        self.gap = gap;
        self
    }

    pub const fn pad(mut self, pad: Space) -> Self {
        self.pad = pad;
        self
    }

    pub const fn align(mut self, align: Align) -> Self {
        self.align = align;
        self
    }

    pub const fn fill(mut self, fill: Fill) -> Self {
        self.fill = fill;
        self
    }

    /// A group: rounded corners on its fill.
    pub const fn card(mut self, fill: Fill) -> Self {
        self.fill = fill;
        self.card = true;
        self
    }
}

/// A view on more than fits: pixels scrolled, of how many.
#[derive(Debug, Clone, Copy, Default)]
struct Scroller {
    offset: i32,
    content: i32,
}

impl Scroller {
    fn furthest(&self, view: i32) -> i32 {
        (self.content - view).max(0)
    }

    /// The thumb, when there is more than fits.
    fn thumb(&self, view: &Rect, t: &Theme) -> Option<Rect> {
        let h = view.h as i32;
        let furthest = self.furthest(h);
        if furthest == 0 {
            return None;
        }
        let long = (h * h / self.content).max(t.thumb_min).min(h);
        let y = view.y + (self.offset.min(furthest) * (h - long)) / furthest;
        Some(Rect::new(
            view.right() - t.thumb_w - 2,
            y,
            t.thumb_w as u32,
            long as u32,
        ))
    }

    /// The offset that puts a thumb grabbed `grab` from its top at `y`.
    fn dragged_to(&self, view: &Rect, t: &Theme, y: i32, grab: i32) -> i32 {
        let h = view.h as i32;
        let furthest = self.furthest(h);
        let long = (h * h / self.content.max(1)).max(t.thumb_min).min(h);
        let room = (h - long).max(1);
        ((y - grab - view.y) * furthest / room).clamp(0, furthest)
    }
}

enum Kind {
    Stack {
        stack: Stack,
        children: Vec<Id>,
    },
    Spacer,
    Label {
        text: String,
        style: Style,
        tone: Tone,
        wrap: bool,
        /// Each line's bytes, from the last layout.
        lines: Vec<(u32, u32)>,
    },
    Button {
        label: String,
        primary: bool,
        enabled: bool,
    },
    Field {
        edit: TextEdit,
        placeholder: String,
        /// Pixels of text scrolled off to the left.
        scroll: i32,
    },
    List {
        rows: Vec<String>,
        selected: Option<usize>,
        scroll: Scroller,
    },
    Area(Area),
    Scroll {
        child: Id,
        scroll: Scroller,
    },
}

/// Lines of text being written, drawn in the mono style: only the lines that show are
/// laid out, so the length of a file does not slow a key. They are not wrapped: a long
/// line scrolls sideways with the caret.
struct Area {
    edit: TextEdit,
    /// Where each line starts, in bytes.
    lines: Vec<u32>,
    scroll: Scroller,
    /// Pixels of text scrolled off to the left.
    scroll_x: i32,
    /// Where Up and Down aim, kept across short lines.
    goal_x: Option<i32>,
}

impl Area {
    fn index_lines(&mut self) {
        self.lines.clear();
        self.lines.push(0);
        let text = self.edit.text().as_bytes();
        self.lines.extend(
            text.iter()
                .enumerate()
                .filter(|(_, &b)| b == b'\n')
                .map(|(i, _)| i as u32 + 1),
        );
    }

    /// The line that holds byte `pos`.
    fn line_of(&self, pos: usize) -> usize {
        self.lines.partition_point(|&s| s as usize <= pos) - 1
    }

    /// Line `i`'s bytes, without its line break.
    fn span(&self, i: usize) -> (usize, usize) {
        let from = self.lines[i] as usize;
        let to = self
            .lines
            .get(i + 1)
            .map_or(self.edit.text().len(), |&next| next as usize - 1);
        (from, to)
    }
}

/// A line as it is drawn: a tab is one space, and a line's carriage return none.
fn shown_line(line: &str) -> alloc::borrow::Cow<'_, str> {
    if line.contains(['\t', '\r']) {
        alloc::borrow::Cow::Owned(line.replace(['\t', '\r'], " "))
    } else {
        alloc::borrow::Cow::Borrowed(line)
    }
}

struct Node {
    kind: Kind,
    rect: Rect,
    parent: Option<Id>,
    flex: i32,
    min: (i32, i32),
    hidden: bool,
}

/// What the pointer is holding.
#[derive(Clone, Copy)]
enum Held {
    /// A button, and whether the pointer is still over it.
    Button(Id, bool),
    Selecting(Id),
    Thumb(Id, i32),
}

/// The rectangles a paint changed, to present.
#[derive(Debug, Clone, Copy, Default)]
pub struct Frame {
    rects: [DamageRect; aricode_gfx::MAX_DAMAGE],
    count: usize,
}

impl Frame {
    /// Nothing to present when this is empty.
    pub fn rects(&self) -> &[DamageRect] {
        &self.rects[..self.count]
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

pub struct Ui {
    nodes: Vec<Node>,
    root: Option<Id>,
    theme: Theme,
    text: Typesetter,
    width: u32,
    height: u32,
    damage: Damage,
    needs_layout: bool,
    focus: Option<Id>,
    /// Whether the window has the keys.
    active: bool,
    default_button: Option<Id>,
    held: Option<Held>,
    keys: KeyDecoder,
    mods: u8,
    actions: VecDeque<Action>,
    /// What Ctrl+C and Ctrl+X last took.
    clipboard: String,
}

fn outset(r: &Rect, by: i32) -> Rect {
    Rect::new(
        r.x - by,
        r.y - by,
        (r.w as i32 + 2 * by) as u32,
        (r.h as i32 + 2 * by) as u32,
    )
}

fn inset(r: &Rect, by: i32) -> Rect {
    Rect::new(
        r.x + by,
        r.y + by,
        (r.w as i32 - 2 * by).max(0) as u32,
        (r.h as i32 - 2 * by).max(0) as u32,
    )
}

impl Ui {
    pub fn new(theme: Theme, width: u32, height: u32) -> Self {
        Self {
            nodes: Vec::new(),
            root: None,
            theme,
            text: Typesetter::new(),
            width,
            height,
            damage: Damage::full(Rect::new(0, 0, width, height)),
            needs_layout: true,
            focus: None,
            active: true,
            default_button: None,
            held: None,
            keys: KeyDecoder::new(),
            mods: 0,
            actions: VecDeque::new(),
            clipboard: String::new(),
        }
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// The whole window takes the new theme at the next paint.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.needs_layout = true;
        self.damage = Damage::full(self.screen());
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The window has this size now.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.needs_layout = true;
        self.damage = Damage::full(self.screen());
    }

    /// The owner wants this size: `answer` gives the window a new buffer, painted
    /// with [`paint_all`](Self::paint_all). If it fails, nothing changed.
    pub fn configure<E>(
        &mut self,
        width: u32,
        height: u32,
        answer: impl FnOnce(&mut Self) -> Result<(), E>,
    ) -> Result<(), E> {
        let was = self.size();
        self.resize(width, height);
        let result = answer(self);
        if result.is_err() {
            self.resize(was.0, was.1);
        }
        result
    }

    fn screen(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    // ---- building ----

    fn add(&mut self, kind: Kind) -> Id {
        let id = Id(self.nodes.len() as u16);
        self.nodes.push(Node {
            kind,
            rect: Rect::new(0, 0, 0, 0),
            parent: None,
            flex: 0,
            min: (0, 0),
            hidden: false,
        });
        self.needs_layout = true;
        id
    }

    fn node(&self, id: Id) -> &Node {
        &self.nodes[id.0 as usize]
    }

    fn node_mut(&mut self, id: Id) -> &mut Node {
        &mut self.nodes[id.0 as usize]
    }

    pub fn stack(&mut self, stack: Stack, children: &[Id]) -> Id {
        let id = self.add(Kind::Stack {
            stack,
            children: children.to_vec(),
        });
        for &c in children {
            self.node_mut(c).parent = Some(id);
        }
        id
    }

    /// Takes what a row or a column has left, as `flex` does.
    pub fn spacer(&mut self) -> Id {
        let id = self.add(Kind::Spacer);
        self.node_mut(id).flex = 1;
        id
    }

    pub fn label(&mut self, text: &str, style: Style, tone: Tone) -> Id {
        self.add(Kind::Label {
            text: String::from(text),
            style,
            tone,
            wrap: false,
            lines: Vec::new(),
        })
    }

    /// A label that breaks into lines at the width it is given.
    pub fn paragraph(&mut self, text: &str, style: Style, tone: Tone) -> Id {
        let id = self.label(text, style, tone);
        if let Kind::Label { wrap, .. } = &mut self.node_mut(id).kind {
            *wrap = true;
        }
        id
    }

    pub fn button(&mut self, label: &str) -> Id {
        self.add(Kind::Button {
            label: String::from(label),
            primary: false,
            enabled: true,
        })
    }

    /// A button in the accent colour, which Enter presses when no widget takes it.
    pub fn default_button(&mut self, label: &str) -> Id {
        let id = self.add(Kind::Button {
            label: String::from(label),
            primary: true,
            enabled: true,
        });
        self.default_button = Some(id);
        id
    }

    /// A one-line field of at most `max` bytes.
    pub fn field(&mut self, max: usize, placeholder: &str) -> Id {
        self.add(Kind::Field {
            edit: TextEdit::new(max, false),
            placeholder: String::from(placeholder),
            scroll: 0,
        })
    }

    pub fn list(&mut self) -> Id {
        self.add(Kind::List {
            rows: Vec::new(),
            selected: None,
            scroll: Scroller::default(),
        })
    }

    /// Lines of text being written, of at most `max` bytes.
    pub fn area(&mut self, max: usize) -> Id {
        let mut area = Area {
            edit: TextEdit::new(max, true),
            lines: Vec::new(),
            scroll: Scroller::default(),
            scroll_x: 0,
            goal_x: None,
        };
        area.index_lines();
        self.add(Kind::Area(area))
    }

    /// Shows `child` scrolled, when it is taller than what this is given.
    pub fn scroll(&mut self, child: Id) -> Id {
        let id = self.add(Kind::Scroll {
            child,
            scroll: Scroller::default(),
        });
        self.node_mut(child).parent = Some(id);
        id
    }

    pub fn set_root(&mut self, id: Id) {
        self.root = Some(id);
        self.needs_layout = true;
    }

    /// How much of a stack's spare room this takes, against its siblings.
    pub fn set_flex(&mut self, id: Id, flex: i32) {
        self.node_mut(id).flex = flex;
        self.needs_layout = true;
    }

    /// The least it is laid out at.
    pub fn set_min(&mut self, id: Id, width: i32, height: i32) {
        self.node_mut(id).min = (width, height);
        self.needs_layout = true;
    }

    // ---- changing what it shows ----

    /// Marks `id`'s area, its focus ring included, to paint again.
    fn dirty(&mut self, id: Id) {
        let r = self.painted_area(id);
        self.damage.add(&r);
    }

    fn painted_area(&self, id: Id) -> Rect {
        let node = self.node(id);
        match node.kind {
            Kind::Button { .. } | Kind::Field { .. } | Kind::Area(_) => {
                outset(&node.rect, self.theme.ring)
            }
            _ => node.rect,
        }
    }

    pub fn set_hidden(&mut self, id: Id, hidden: bool) {
        if self.node(id).hidden != hidden {
            self.dirty(id);
            let node = self.node_mut(id);
            node.hidden = hidden;
            node.rect = Rect::new(0, 0, 0, 0);
            self.needs_layout = true;
            if hidden && self.focus.is_some_and(|f| self.is_within(f, id)) {
                self.focus = None;
            }
        }
    }

    fn is_within(&self, mut id: Id, ancestor: Id) -> bool {
        loop {
            if id == ancestor {
                return true;
            }
            match self.node(id).parent {
                Some(p) => id = p,
                None => return false,
            }
        }
    }

    /// A button shown in the accent colour, or back.
    pub fn set_primary(&mut self, id: Id, on: bool) {
        if let Kind::Button { primary, .. } = &mut self.node_mut(id).kind {
            if *primary != on {
                *primary = on;
                self.dirty(id);
            }
        }
    }

    /// A label's, a button's, a field's or an area's text; an area's caret goes to its
    /// start. Saying the same again costs nothing.
    pub fn set_text(&mut self, id: Id, new: &str) {
        let mut changed = true;
        match &mut self.node_mut(id).kind {
            Kind::Label { text, .. } | Kind::Button { label: text, .. } => {
                if text == new {
                    changed = false;
                } else {
                    text.clear();
                    text.push_str(new);
                }
            }
            Kind::Field { edit, scroll, .. } => {
                if edit.text() == new {
                    changed = false;
                } else {
                    edit.set_text(new);
                    *scroll = 0;
                }
            }
            Kind::Area(area) => {
                if area.edit.text() == new {
                    changed = false;
                } else {
                    area.edit.set_text(new);
                    area.edit.move_to(0, false);
                    area.index_lines();
                    (area.scroll.offset, area.scroll_x, area.goal_x) = (0, 0, None);
                }
            }
            _ => changed = false,
        }
        if changed {
            self.dirty(id);
            self.reveal_caret(id);
            self.needs_layout = true;
        }
    }

    pub fn set_tone(&mut self, id: Id, new: Tone) {
        if let Kind::Label { tone, .. } = &mut self.node_mut(id).kind {
            if *tone != new {
                *tone = new;
                self.dirty(id);
            }
        }
    }

    pub fn set_enabled(&mut self, id: Id, on: bool) {
        if let Kind::Button { enabled, .. } = &mut self.node_mut(id).kind {
            if *enabled != on {
                *enabled = on;
                self.dirty(id);
                if !on && self.focus == Some(id) {
                    self.focus = None;
                }
            }
        }
    }

    /// A field's text.
    pub fn text(&self, id: Id) -> &str {
        match &self.node(id).kind {
            Kind::Field { edit, .. } => edit.text(),
            Kind::Area(area) => area.edit.text(),
            Kind::Label { text, .. } | Kind::Button { label: text, .. } => text,
            _ => "",
        }
    }

    /// A list's rows, keeping the selection inside; the first is chosen if none was.
    pub fn set_rows<'a>(&mut self, id: Id, new: impl IntoIterator<Item = &'a str>) {
        if let Kind::List { rows, selected, .. } = &mut self.node_mut(id).kind {
            rows.clear();
            rows.extend(new.into_iter().map(String::from));
            *selected = match *selected {
                _ if rows.is_empty() => None,
                Some(s) => Some(s.min(rows.len() - 1)),
                None => Some(0),
            };
            let height = self.theme.row_h * self.list_len(id) as i32;
            if let Kind::List { scroll, .. } = &mut self.node_mut(id).kind {
                scroll.content = height;
            }
            self.reveal_selected(id);
            self.dirty(id);
            self.needs_layout = true;
        }
    }

    /// A list's row.
    pub fn row(&self, id: Id, row: usize) -> &str {
        match &self.node(id).kind {
            Kind::List { rows, .. } => rows.get(row).map_or("", String::as_str),
            _ => "",
        }
    }

    pub fn list_len(&self, id: Id) -> usize {
        match &self.node(id).kind {
            Kind::List { rows, .. } => rows.len(),
            _ => 0,
        }
    }

    pub fn selected(&self, id: Id) -> Option<usize> {
        match &self.node(id).kind {
            Kind::List { selected, .. } => *selected,
            _ => None,
        }
    }

    /// Selects a row, or none, without telling the program it was the owner.
    pub fn select(&mut self, id: Id, row: Option<usize>) {
        if let Kind::List { rows, selected, .. } = &mut self.node_mut(id).kind {
            let row = row.filter(|&r| r < rows.len());
            if *selected != row {
                *selected = row;
                self.reveal_selected(id);
                self.dirty(id);
            }
        }
    }

    /// Gives `id` the keys.
    pub fn focus(&mut self, id: Id) {
        if self.focus != Some(id) {
            if let Some(old) = self.focus {
                self.dirty(old);
            }
            self.focus = Some(id);
            self.dirty(id);
            self.reveal(id);
        }
    }

    pub fn focused(&self) -> Option<Id> {
        self.focus
    }

    /// What the owner did since the last call.
    pub fn next_action(&mut self) -> Option<Action> {
        self.actions.pop_front()
    }

    // ---- events ----

    /// One event of the window. `Configure` is the program's: it answers with a buffer.
    pub fn handle(&mut self, event: Event) {
        match event {
            Event::Key(byte) => {
                for key in self.keys.feed(byte).into_iter().flatten() {
                    self.on_key(key);
                }
            }
            Event::KeyCode { mods, .. } => self.mods = mods,
            Event::Click { x, y, button: LEFT } => self.press(x, y),
            Event::Motion { x, y, .. } => self.pointer_to(x, y),
            Event::Release { x, y, button: LEFT } => self.release(x, y),
            Event::Wheel { x, y, delta } => self.wheel(x, y, delta),
            Event::Focus(on) => {
                self.active = on;
                if !on {
                    self.mods = 0;
                }
                if let Some(id) = self.focus {
                    self.dirty(id);
                }
            }
            _ => {}
        }
    }

    fn on_key(&mut self, key: Key) {
        let shift = self.mods & mods::SHIFT != 0;
        let writes_tabs = self
            .focus
            .is_some_and(|id| matches!(self.node(id).kind, Kind::Area(_)));
        match key {
            Key::Tab if !writes_tabs || shift => return self.focus_next(shift),
            Key::Escape => return self.actions.push_back(Action::Cancel),
            _ => {}
        }
        let taken = self.focus.is_some_and(|id| self.key_for(id, key, shift));
        if taken {
            return;
        }
        match key {
            Key::Enter => {
                if let Some(id) = self.default_button.filter(|&b| self.is_enabled(b)) {
                    self.actions.push_back(Action::Pressed(id));
                }
            }
            Key::Ctrl(letter) => self.actions.push_back(Action::Shortcut(letter)),
            _ => {}
        }
    }

    fn is_enabled(&self, id: Id) -> bool {
        !self.node(id).hidden
            && match self.node(id).kind {
                Kind::Button { enabled, .. } => enabled,
                _ => true,
            }
    }

    /// Whether `id` took the key.
    fn key_for(&mut self, id: Id, key: Key, shift: bool) -> bool {
        match self.node(id).kind {
            Kind::Button { .. } => {
                let press = matches!(key, Key::Enter | Key::Char(' '));
                if press {
                    self.actions.push_back(Action::Pressed(id));
                }
                press
            }
            Kind::List { .. } => self.key_for_list(id, key),
            Kind::Field { .. } | Kind::Area(_) => self.key_for_edit(id, key, shift),
            _ => false,
        }
    }

    fn key_for_list(&mut self, id: Id, key: Key) -> bool {
        let Kind::List { rows, selected, .. } = &self.node(id).kind else {
            return false;
        };
        let last = rows.len().checked_sub(1);
        let (Some(last), now) = (last, *selected) else {
            return false;
        };
        let at = now.unwrap_or(0);
        let to = match key {
            Key::Up => at.saturating_sub(1),
            Key::Down => (at + 1).min(last),
            Key::Home => 0,
            Key::End => last,
            Key::Enter => {
                if now.is_some() {
                    self.actions.push_back(Action::Activated(id, at));
                }
                return true;
            }
            Key::Delete => {
                if now.is_some() {
                    self.actions.push_back(Action::DeleteRow(id, at));
                }
                return true;
            }
            _ => return false,
        };
        self.pick_row(id, to);
        true
    }

    /// The owner chose `row`.
    fn pick_row(&mut self, id: Id, row: usize) {
        if self.selected(id) != Some(row) {
            self.select(id, Some(row));
            self.actions.push_back(Action::Selected(id, row));
        }
    }

    fn edit_mut(&mut self, id: Id) -> Option<&mut TextEdit> {
        match &mut self.node_mut(id).kind {
            Kind::Field { edit, .. } => Some(edit),
            Kind::Area(area) => Some(&mut area.edit),
            _ => None,
        }
    }

    fn key_for_edit(&mut self, id: Id, key: Key, shift: bool) -> bool {
        let word = self.mods & mods::CTRL != 0;
        let lines = matches!(self.node(id).kind, Kind::Area(_));
        let mut clipboard = None;
        let mut paste = false;
        if lines && matches!(key, Key::Up | Key::Down) {
            self.move_line(id, key == Key::Up, shift);
            self.reveal_caret(id);
            self.dirty(id);
            return true;
        }
        let Some(edit) = self.edit_mut(id) else {
            return false;
        };
        let mut edited = false;
        match key {
            Key::Char(c) => {
                let mut one = [0u8; 4];
                edited = edit.insert(c.encode_utf8(&mut one));
            }
            Key::Enter if lines => edited = edit.insert("\n"),
            Key::Tab => edited = edit.insert("\t"),
            Key::Backspace => edited = edit.backspace(),
            Key::Delete => edited = edit.delete(),
            Key::Left => {
                let (from, to) = edit.selection();
                let at = match (word, shift) {
                    (true, _) => edit.word_before(edit.caret()),
                    (false, false) if from != to => from,
                    _ => edit.before(edit.caret()),
                };
                edit.move_to(at, shift);
            }
            Key::Right => {
                let (from, to) = edit.selection();
                let at = match (word, shift) {
                    (true, _) => edit.word_after(edit.caret()),
                    (false, false) if from != to => to,
                    _ => edit.after(edit.caret()),
                };
                edit.move_to(at, shift);
            }
            Key::Home => edit.move_to(edit.line_start(edit.caret()), shift),
            Key::End => edit.move_to(edit.line_end(edit.caret()), shift),
            Key::Enter => {
                self.actions.push_back(Action::Submitted(id));
                return true;
            }
            Key::Ctrl(b'a') => edit.select_all(),
            Key::Ctrl(b'c') => {
                if !edit.selected().is_empty() {
                    clipboard = Some(String::from(edit.selected()));
                }
            }
            Key::Ctrl(b'x') => {
                if !edit.selected().is_empty() {
                    clipboard = Some(String::from(edit.selected()));
                    edited = edit.insert("");
                }
            }
            Key::Ctrl(b'v') => paste = true,
            _ => return false,
        }
        if let Some(taken) = clipboard {
            self.clipboard = taken;
        }
        if paste {
            let text = core::mem::take(&mut self.clipboard);
            if let Some(edit) = self.edit_mut(id) {
                edited = edit.insert(&text);
            }
            self.clipboard = text;
        }
        if let Kind::Area(area) = &mut self.node_mut(id).kind {
            area.goal_x = None;
            if edited {
                area.index_lines();
            }
        }
        self.reveal_caret(id);
        self.dirty(id);
        if edited {
            self.actions.push_back(Action::Edited(id));
        }
        true
    }

    /// Up or Down in an area: to the character nearest where the caret was aiming.
    fn move_line(&mut self, id: Id, up: bool, extend: bool) {
        let mono = self.theme.style(Style::Mono);
        let Kind::Area(area) = &self.node(id).kind else {
            return;
        };
        let caret = area.edit.caret();
        let line = area.line_of(caret);
        let (from, _) = area.span(line);
        let goal = area.goal_x.unwrap_or_else(|| {
            let (mut x, text) = (0, &area.edit.text()[from..caret]);
            self.text
                .offsets(mono.face, mono.size, text, |_, at| x = at);
            x
        });
        let to = match up {
            true if line == 0 => 0,
            false if line + 1 == area.lines.len() => area.edit.text().len(),
            _ => {
                let i = if up { line - 1 } else { line + 1 };
                self.offset_in_line(area, i, goal)
            }
        };
        if let Kind::Area(area) = &mut self.node_mut(id).kind {
            area.edit.move_to(to, extend);
            area.goal_x = Some(goal);
        }
    }

    /// The character boundary of line `i` nearest `want` pixels from its start.
    fn offset_in_line(&self, area: &Area, i: usize, want: i32) -> usize {
        let mono = self.theme.style(Style::Mono);
        let (from, to) = area.span(i);
        let line = &area.edit.text()[from..to];
        let (mut best, mut distance) = (from, i32::MAX);
        self.text.offsets(mono.face, mono.size, line, |offset, x| {
            if (x - want).abs() < distance {
                (best, distance) = (from + offset, (x - want).abs());
            }
        });
        best
    }

    /// The character nearest the pointer in an area.
    fn area_offset_at(&self, id: Id, x: i32, y: i32) -> usize {
        let t = &self.theme;
        let mono = t.style(Style::Mono);
        let inner = self.field_inner(id);
        let Kind::Area(area) = &self.node(id).kind else {
            return 0;
        };
        let row = (y - inner.y - t.control_gap + area.scroll.offset) / mono.leading;
        let row = row.clamp(0, area.lines.len() as i32 - 1) as usize;
        self.offset_in_line(area, row, x - inner.x + area.scroll_x)
    }

    /// Who takes the keys, in the order they are laid out.
    fn focusable(&self, id: Id, out: &mut Vec<Id>) {
        let node = self.node(id);
        if node.hidden {
            return;
        }
        match &node.kind {
            Kind::Stack { children, .. } => {
                for &c in children {
                    self.focusable(c, out);
                }
            }
            Kind::Scroll { child, .. } => self.focusable(*child, out),
            Kind::Button { enabled: true, .. }
            | Kind::Field { .. }
            | Kind::List { .. }
            | Kind::Area(_) => out.push(id),
            _ => {}
        }
    }

    fn focus_next(&mut self, back: bool) {
        let Some(root) = self.root else { return };
        let mut order = Vec::new();
        self.focusable(root, &mut order);
        if order.is_empty() {
            return;
        }
        let n = order.len();
        let at = self.focus.and_then(|f| order.iter().position(|&o| o == f));
        let to = match (at, back) {
            (Some(i), false) => (i + 1) % n,
            (Some(i), true) => (i + n - 1) % n,
            (None, false) => 0,
            (None, true) => n - 1,
        };
        self.focus(order[to]);
    }

    fn hit(&self, id: Id, x: i32, y: i32) -> Option<Id> {
        let node = self.node(id);
        if node.hidden {
            return None;
        }
        match &node.kind {
            Kind::Stack { children, .. } => children.iter().rev().find_map(|&c| self.hit(c, x, y)),
            Kind::Scroll { child, scroll } => {
                if !node.rect.contains(x, y) {
                    return None;
                }
                let on_thumb = scroll
                    .thumb(&node.rect, &self.theme)
                    .is_some_and(|t| t.contains(x, y));
                if on_thumb {
                    Some(id)
                } else {
                    self.hit(*child, x, y).or(Some(id))
                }
            }
            _ => node.rect.contains(x, y).then_some(id),
        }
    }

    fn press(&mut self, x: i32, y: i32) {
        let Some(id) = self.root.and_then(|r| self.hit(r, x, y)) else {
            return;
        };
        let theme = self.theme;
        let rect = self.node(id).rect;
        match &self.node(id).kind {
            Kind::Button { enabled: true, .. } => {
                self.held = Some(Held::Button(id, true));
                self.dirty(id);
            }
            Kind::Field { .. } => {
                self.focus(id);
                let at = self.field_offset_at(id, x);
                let extend = self.mods & mods::SHIFT != 0;
                if let Kind::Field { edit, .. } = &mut self.node_mut(id).kind {
                    edit.move_to(at, extend);
                }
                self.held = Some(Held::Selecting(id));
                self.reveal_caret(id);
                self.dirty(id);
            }
            Kind::Area(area) => {
                let thumb = area
                    .scroll
                    .thumb(&rect, &theme)
                    .filter(|t| t.contains(x, y));
                if let Some(t) = thumb {
                    self.held = Some(Held::Thumb(id, y - t.y));
                    return;
                }
                self.focus(id);
                let at = self.area_offset_at(id, x, y);
                let extend = self.mods & mods::SHIFT != 0;
                if let Kind::Area(area) = &mut self.node_mut(id).kind {
                    area.edit.move_to(at, extend);
                    area.goal_x = None;
                }
                self.held = Some(Held::Selecting(id));
                self.reveal_caret(id);
                self.dirty(id);
            }
            Kind::List {
                rows,
                selected,
                scroll,
            } => {
                let thumb = scroll.thumb(&rect, &theme).filter(|t| t.contains(x, y));
                if let Some(t) = thumb {
                    self.held = Some(Held::Thumb(id, y - t.y));
                    return;
                }
                let row = (y - rect.y + scroll.offset) / theme.row_h;
                let row = usize::try_from(row).ok().filter(|&r| r < rows.len());
                let selected = *selected;
                self.focus(id);
                match row {
                    Some(row) if selected == Some(row) => {
                        self.actions.push_back(Action::Activated(id, row));
                    }
                    Some(row) => self.pick_row(id, row),
                    None => {}
                }
            }
            Kind::Scroll { scroll, .. } => {
                if let Some(t) = scroll.thumb(&rect, &theme).filter(|t| t.contains(x, y)) {
                    self.held = Some(Held::Thumb(id, y - t.y));
                }
            }
            _ => {}
        }
    }

    fn pointer_to(&mut self, x: i32, y: i32) {
        match self.held {
            Some(Held::Button(id, was)) => {
                let over = self.node(id).rect.contains(x, y);
                if over != was {
                    self.held = Some(Held::Button(id, over));
                    self.dirty(id);
                }
            }
            Some(Held::Selecting(id)) => {
                let at = match self.node(id).kind {
                    Kind::Area(_) => self.area_offset_at(id, x, y),
                    _ => self.field_offset_at(id, x),
                };
                if let Some(edit) = self.edit_mut(id) {
                    edit.move_to(at, true);
                }
                self.reveal_caret(id);
                self.dirty(id);
            }
            Some(Held::Thumb(id, grab)) => {
                let theme = self.theme;
                let rect = self.node(id).rect;
                let to = match &self.node(id).kind {
                    Kind::List { scroll, .. } | Kind::Scroll { scroll, .. } => {
                        scroll.dragged_to(&rect, &theme, y, grab)
                    }
                    Kind::Area(area) => area.scroll.dragged_to(&rect, &theme, y, grab),
                    _ => return,
                };
                self.scroll_to(id, to);
            }
            None => {}
        }
    }

    fn release(&mut self, x: i32, y: i32) {
        self.pointer_to(x, y);
        if let Some(Held::Button(id, over)) = self.held {
            self.dirty(id);
            if over {
                self.actions.push_back(Action::Pressed(id));
            }
        }
        self.held = None;
    }

    fn wheel(&mut self, x: i32, y: i32, delta: i32) {
        let Some(mut id) = self.root.and_then(|r| self.hit(r, x, y)) else {
            return;
        };
        let step = self.theme.wheel_rows * self.theme.row_h;
        loop {
            let scroll = match &self.node(id).kind {
                Kind::List { scroll, .. } | Kind::Scroll { scroll, .. } => Some(scroll),
                Kind::Area(area) => Some(&area.scroll),
                _ => None,
            };
            if let Some(scroll) = scroll {
                let to = scroll.offset - delta * step;
                return self.scroll_to(id, to);
            }
            match self.node(id).parent {
                Some(p) => id = p,
                None => return,
            }
        }
    }

    fn scroll_to(&mut self, id: Id, to: i32) {
        let view = self.node(id).rect.h as i32;
        let mut relayout = false;
        match &mut self.node_mut(id).kind {
            Kind::List { scroll, .. } => {
                let to = to.clamp(0, scroll.furthest(view));
                if scroll.offset == to {
                    return;
                }
                scroll.offset = to;
            }
            Kind::Scroll { scroll, .. } => {
                let to = to.clamp(0, scroll.furthest(view));
                if scroll.offset == to {
                    return;
                }
                scroll.offset = to;
                relayout = true;
            }
            Kind::Area(area) => {
                let to = to.clamp(0, area.scroll.furthest(view));
                if area.scroll.offset == to {
                    return;
                }
                area.scroll.offset = to;
            }
            _ => return,
        }
        self.needs_layout |= relayout;
        self.dirty(id);
    }

    /// Scrolls any view holding `id` so it shows.
    fn reveal(&mut self, id: Id) {
        let rect = self.painted_area(id);
        let mut at = self.node(id).parent;
        while let Some(up) = at {
            let view = self.node(up).rect;
            if let Kind::Scroll { scroll, .. } = self.node(up).kind {
                if rect.y < view.y {
                    self.scroll_to(up, scroll.offset - (view.y - rect.y));
                } else if rect.bottom() > view.bottom() {
                    self.scroll_to(up, scroll.offset + rect.bottom() - view.bottom());
                }
            }
            at = self.node(up).parent;
        }
    }

    fn reveal_selected(&mut self, id: Id) {
        let row_h = self.theme.row_h;
        let view = self.node(id).rect.h as i32;
        if let Kind::List {
            selected, scroll, ..
        } = &self.node(id).kind
        {
            let Some(row) = *selected else { return };
            let (top, offset) = (row as i32 * row_h, scroll.offset);
            let to = if top < offset {
                top
            } else if top + row_h > offset + view {
                top + row_h - view
            } else {
                offset
            };
            self.scroll_to(id, to);
        }
    }

    // ---- fields ----

    /// The space inside a field for its text.
    fn field_inner(&self, id: Id) -> Rect {
        let r = self.node(id).rect;
        let pad = self.theme.control_pad;
        Rect::new(r.x + pad, r.y, (r.w as i32 - 2 * pad).max(1) as u32, r.h)
    }

    /// The character boundary nearest the pointer at `x`.
    fn field_offset_at(&self, id: Id, x: i32) -> usize {
        let inner = self.field_inner(id);
        let body = self.theme.style(Style::Body);
        let Kind::Field { edit, scroll, .. } = &self.node(id).kind else {
            return 0;
        };
        let want = x - inner.x + scroll;
        let (mut best, mut distance) = (0, i32::MAX);
        self.text
            .offsets(body.face, body.size, edit.text(), |offset, at| {
                if (at - want).abs() < distance {
                    (best, distance) = (offset, (at - want).abs());
                }
            });
        best
    }

    /// Scrolls an area so its caret shows, both ways.
    fn reveal_area_caret(&mut self, id: Id) {
        let t = self.theme;
        let mono = t.style(Style::Mono);
        let room = self.field_inner(id).w as i32 - 1;
        let view = self.node(id).rect.h as i32;
        let Kind::Area(area) = &mut self.node_mut(id).kind else {
            return;
        };
        area.scroll.content = area.lines.len() as i32 * mono.leading + 2 * t.control_gap;
        let caret = area.edit.caret();
        let line = area.line_of(caret);
        let (top, offset) = (line as i32 * mono.leading, area.scroll.offset);
        let to = if top < offset {
            top
        } else if top + mono.leading + 2 * t.control_gap > offset + view {
            top + mono.leading + 2 * t.control_gap - view
        } else {
            offset
        };
        area.scroll.offset = to.clamp(0, area.scroll.furthest(view));
        let from = area.span(line).0;
        let (mut x, was) = (0, area.scroll_x);
        let text = core::mem::take(&mut self.text);
        if let Kind::Area(area) = &self.node(id).kind {
            text.offsets(
                mono.face,
                mono.size,
                &area.edit.text()[from..caret],
                |_, at| x = at,
            );
        }
        self.text = text;
        let sideways = if x < was {
            x
        } else if x - was > room {
            x - room
        } else {
            was
        };
        if let Kind::Area(area) = &mut self.node_mut(id).kind {
            area.scroll_x = sideways;
        }
    }

    /// Scrolls a field's or an area's text so the caret shows.
    fn reveal_caret(&mut self, id: Id) {
        if matches!(self.node(id).kind, Kind::Area(_)) {
            return self.reveal_area_caret(id);
        }
        let Kind::Field { edit, scroll, .. } = &self.node(id).kind else {
            return;
        };
        let body = self.theme.style(Style::Body);
        let room = self.field_inner(id).w as i32 - 1;
        let (mut at, mut end) = (0, 0);
        let caret = edit.caret();
        self.text
            .offsets(body.face, body.size, edit.text(), |offset, x| {
                if offset == caret {
                    at = x;
                }
                end = x;
            });
        let to = if at < *scroll {
            at
        } else if at - *scroll > room {
            at - room
        } else {
            *scroll
        };
        let to = to.min((end - room).max(0)).max(0);
        if let Kind::Field { scroll, .. } = &mut self.node_mut(id).kind {
            *scroll = to;
        }
    }

    // ---- layout ----

    /// Where `text` breaks into lines of `width`, in bytes: at a line break, else at the
    /// last space, else where a word no longer fits.
    fn wrapped(&self, text: &str, t: Type, width: i32) -> Vec<(u32, u32)> {
        let mut lines = Vec::new();
        let bytes = text.as_bytes();
        let (mut start, mut start_x) = (0usize, 0);
        // The last space of this line, with where what follows it starts.
        let mut space: Option<(usize, i32)> = None;
        let (mut prev, mut prev_x) = (0usize, 0);
        self.text.offsets(t.face, t.size, text, |offset, x| {
            if let Some((at, after)) = &mut space {
                if *at + 1 == offset {
                    *after = x;
                }
            }
            if offset > start && bytes[prev] == b'\n' {
                lines.push((start as u32, prev as u32));
                (start, start_x, space) = (offset, x, None);
            } else if offset > start && x - start_x > width {
                let (end, next, next_x) = match space {
                    Some((at, after)) if at < offset => (at, at + 1, after),
                    _ if prev > start => (prev, prev, prev_x),
                    _ => (offset, offset, x),
                };
                lines.push((start as u32, end as u32));
                (start, start_x, space) = (next, next_x, None);
            }
            if bytes.get(offset) == Some(&b' ') && offset >= start {
                space = Some((offset, x));
            }
            (prev, prev_x) = (offset, x);
        });
        lines.push((start as u32, bytes.len() as u32));
        lines
    }

    /// What `id` wants, given at most `max_w`.
    fn measure(&self, id: Id, max_w: i32) -> (i32, i32) {
        let node = self.node(id);
        if node.hidden {
            return (0, 0);
        }
        let t = &self.theme;
        let (w, h) = match &node.kind {
            Kind::Spacer => (0, 0),
            Kind::Label {
                text, style, wrap, ..
            } => {
                let ty = t.style(*style);
                let width = self.text.width(ty.face, ty.size, text);
                if *wrap && width > max_w {
                    let lines = self.wrapped(text, ty, max_w);
                    (max_w, ty.leading * lines.len() as i32)
                } else {
                    (width, ty.leading)
                }
            }
            Kind::Button { label, .. } => {
                let ty = t.style(Style::Body);
                let width = self.text.width(ty.face, ty.size, label) + 2 * t.control_pad;
                (width.max(t.button_min), t.control_h)
            }
            Kind::Field { .. } => (t.field_min, t.control_h),
            Kind::Area(_) => (t.field_min, t.list_rows * t.row_h),
            Kind::List { rows, .. } => {
                let shown = (rows.len() as i32).clamp(1, t.list_rows);
                (t.field_min, shown * t.row_h)
            }
            Kind::Scroll { child, .. } => (self.measure(*child, max_w).0, 0),
            Kind::Stack { stack, children } => {
                let pad = t.space(stack.pad);
                let gap = t.space(stack.gap);
                let inner = (max_w - 2 * pad).max(0);
                let (mut along, mut across, mut shown) = (0, 0, 0);
                for &c in children {
                    if self.node(c).hidden {
                        continue;
                    }
                    let (cw, ch) = self.measure(c, inner);
                    let (a, b) = match stack.axis {
                        Axis::Column => (ch, cw),
                        Axis::Row => (cw, ch),
                    };
                    along += a;
                    across = across.max(b);
                    shown += 1;
                }
                along += gap * (shown - 1).max(0);
                match stack.axis {
                    Axis::Column => (across + 2 * pad, along + 2 * pad),
                    Axis::Row => (along + 2 * pad, across + 2 * pad),
                }
            }
        };
        (w.max(node.min.0), h.max(node.min.1))
    }

    /// Puts `id` at `rect`, marking both places if it moved.
    fn place(&mut self, id: Id, rect: Rect) {
        let old = self.painted_area(id);
        if self.node(id).rect != rect {
            self.damage.add(&old);
            self.node_mut(id).rect = rect;
            self.dirty(id);
        }
        match &self.node(id).kind {
            Kind::Stack { .. } => self.place_stack(id, rect),
            Kind::Scroll { child, .. } => {
                let child = *child;
                let (_, content) = self.measure(child, rect.w as i32);
                let view = rect.h as i32;
                let mut offset = 0;
                if let Kind::Scroll { scroll, .. } = &mut self.node_mut(id).kind {
                    scroll.content = content;
                    scroll.offset = scroll.offset.clamp(0, scroll.furthest(view));
                    offset = scroll.offset;
                }
                self.place(
                    child,
                    Rect::new(rect.x, rect.y - offset, rect.w, content.max(view) as u32),
                );
            }
            Kind::List { .. } => {
                self.reveal_selected(id);
            }
            Kind::Field { .. } | Kind::Area(_) => self.reveal_caret(id),
            Kind::Label {
                text, style, wrap, ..
            } => {
                let ty = self.theme.style(*style);
                let lines = if *wrap {
                    self.wrapped(text, ty, rect.w as i32)
                } else {
                    alloc::vec![(0, text.len() as u32)]
                };
                if let Kind::Label { lines: to, .. } = &mut self.node_mut(id).kind {
                    *to = lines;
                }
            }
            _ => {}
        }
    }

    fn place_stack(&mut self, id: Id, rect: Rect) {
        let Kind::Stack { stack, children } = &mut self.node_mut(id).kind else {
            return;
        };
        let stack = *stack;
        let children = core::mem::take(children);
        let pad = self.theme.space(stack.pad);
        let gap = self.theme.space(stack.gap);
        let inner = inset(&rect, pad);
        let column = stack.axis == Axis::Column;
        let (room, across) = if column {
            (inner.h as i32, inner.w as i32)
        } else {
            (inner.w as i32, inner.h as i32)
        };
        let mut sizes = Vec::with_capacity(children.len());
        let (mut used, mut flex, mut shown) = (0, 0, 0);
        for &c in &children {
            if self.node(c).hidden {
                sizes.push((0, 0));
                continue;
            }
            let (w, h) = self.measure(c, inner.w as i32);
            let (a, b) = if column { (h, w) } else { (w, h) };
            used += a;
            flex += self.node(c).flex;
            shown += 1;
            sizes.push((a, b));
        }
        used += gap * (shown - 1).max(0);
        let mut spare = (room - used).max(0);
        let mut flex_left = flex;
        let mut along = if column { inner.y } else { inner.x };
        for (i, &c) in children.iter().enumerate() {
            if self.node(c).hidden {
                continue;
            }
            let (mut a, b) = sizes[i];
            let share = self.node(c).flex;
            if share > 0 && flex_left > 0 {
                let extra = spare * share / flex_left;
                a += extra;
                spare -= extra;
                flex_left -= share;
            }
            let (cross_at, b) = match stack.align {
                Align::Stretch => (0, across),
                Align::Start => (0, b.min(across)),
                Align::Center => ((across - b).max(0) / 2, b.min(across)),
                Align::End => ((across - b).max(0), b.min(across)),
            };
            let r = if column {
                Rect::new(inner.x + cross_at, along, b.max(0) as u32, a.max(0) as u32)
            } else {
                Rect::new(along, inner.y + cross_at, a.max(0) as u32, b.max(0) as u32)
            };
            self.place(c, r);
            along += a + gap;
        }
        if let Kind::Stack { children: back, .. } = &mut self.node_mut(id).kind {
            *back = children;
        }
    }

    fn layout(&mut self) {
        self.needs_layout = false;
        if let Some(root) = self.root {
            let screen = self.screen();
            self.place(root, screen);
        }
    }

    // ---- painting ----

    /// Paints what changed, and answers it. Empty: there is nothing to present.
    pub fn paint(&mut self, canvas: &mut Canvas) -> Frame {
        if self.needs_layout {
            self.layout();
        }
        let mut frame = Frame::default();
        if let Some(root) = self.root {
            let mut text = core::mem::take(&mut self.text);
            for r in self.damage.rects() {
                // Rounded corners blend with what is below: it starts clean each time.
                let mut part = canvas.clipped(r);
                part.fill(self.theme.window);
                self.paint_node(root, &mut part, &mut text);
                frame.rects[frame.count] = DamageRect {
                    x: r.x as u16,
                    y: r.y as u16,
                    width: r.w as u16,
                    height: r.h as u16,
                };
                frame.count += 1;
            }
            self.text = text;
        }
        self.damage.clear();
        frame
    }

    /// Paints everything: a new buffer, on a new size.
    pub fn paint_all(&mut self, canvas: &mut Canvas) {
        self.damage = Damage::full(self.screen());
        self.paint(canvas);
    }

    fn paint_node(&self, id: Id, c: &mut Canvas, text: &mut Typesetter) {
        let node = self.node(id);
        let t = &self.theme;
        if node.hidden || !self.painted_area(id).intersects(&c.clip()) {
            return;
        }
        let r = node.rect;
        let focused = self.active && self.focus == Some(id);
        match &node.kind {
            Kind::Spacer => {}
            Kind::Stack { stack, children } => {
                if let Some(color) = t.fill(stack.fill) {
                    if stack.card {
                        c.rounded(&r, t.card_radius, color);
                    } else {
                        c.fill_rect(&r, color);
                    }
                }
                for &child in children {
                    self.paint_node(child, c, text);
                }
            }
            Kind::Scroll { child, scroll } => {
                let mut view = c.clipped(&r);
                self.paint_node(*child, &mut view, text);
                if let Some(thumb) = scroll.thumb(&r, t) {
                    view.rounded(&thumb, t.thumb_w / 2, t.thumb);
                }
            }
            Kind::Label {
                text: s,
                style,
                tone,
                lines,
                ..
            } => {
                let ty = t.style(*style);
                let mut inside = c.clipped(&r);
                let baseline = baseline_in(text, ty, r.y, ty.leading);
                for (i, &(from, to)) in lines.iter().enumerate() {
                    let line = s.get(from as usize..to as usize).unwrap_or("");
                    let y = baseline + i as i32 * ty.leading;
                    text.draw(
                        &mut inside,
                        (ty.face, ty.size),
                        line,
                        (r.x, y),
                        t.tone(*tone),
                    );
                }
            }
            Kind::Button {
                label,
                primary,
                enabled,
            } => {
                if focused {
                    c.rounded(&outset(&r, t.ring), t.radius + t.ring, t.focus);
                }
                let down = matches!(self.held, Some(Held::Button(h, true)) if h == id);
                let ink = if *primary && *enabled {
                    c.rounded(&r, t.radius, if down { t.accent_pressed } else { t.accent });
                    t.on_accent
                } else {
                    c.rounded(&r, t.radius, t.control_edge);
                    let body = if down { t.control_pressed } else { t.control };
                    c.rounded(&inset(&r, 1), t.radius - 1, body);
                    if *enabled {
                        t.primary
                    } else {
                        t.tertiary
                    }
                };
                let ty = t.style(Style::Body);
                let width = text.width(ty.face, ty.size, label);
                let x = r.x + (r.w as i32 - width) / 2;
                let y = baseline_in(text, ty, r.y, r.h as i32);
                let mut inside = c.clipped(&r);
                text.draw(&mut inside, (ty.face, ty.size), label, (x, y), ink);
            }
            Kind::Field {
                edit,
                placeholder,
                scroll,
            } => {
                if focused {
                    c.rounded(&outset(&r, t.ring), t.radius + t.ring, t.focus);
                }
                c.rounded(&r, t.radius, t.control_edge);
                c.rounded(&inset(&r, 1), t.radius - 1, t.control);
                let inner = self.field_inner(id);
                let ty = t.style(Style::Body);
                let y = baseline_in(text, ty, r.y, r.h as i32);
                let mut inside = c.clipped(&inner);
                if edit.text().is_empty() && !focused {
                    let at = (inner.x, y);
                    text.draw(&mut inside, (ty.face, ty.size), placeholder, at, t.tertiary);
                    return;
                }
                // Glyphs are drawn from the first one that shows: a long text is
                // not walked for what is off to the left.
                let (from, to) = edit.selection();
                let caret = edit.caret();
                let (mut first, mut first_x) = (None, 0);
                let (mut from_x, mut to_x, mut caret_x) = (0, 0, 0);
                let source = &*text;
                source.offsets(ty.face, ty.size, edit.text(), |offset, x| {
                    if first.is_none() && x >= *scroll - ty.size as i32 {
                        (first, first_x) = (Some(offset), x);
                    }
                    if offset == from {
                        from_x = x;
                    }
                    if offset == to {
                        to_x = x;
                    }
                    if offset == caret {
                        caret_x = x;
                    }
                });
                if from != to {
                    let color = if focused {
                        t.text_selection
                    } else {
                        t.quiet_selection
                    };
                    let band = Rect::new(
                        inner.x + from_x - scroll,
                        inset(&r, 2).y,
                        (to_x - from_x) as u32,
                        inset(&r, 2).h,
                    );
                    inside.fill_rect(&band, color);
                }
                let shown = edit.text().get(first.unwrap_or(0)..).unwrap_or("");
                let at = (inner.x + first_x - scroll, y);
                text.draw(&mut inside, (ty.face, ty.size), shown, at, t.primary);
                if focused && from == to {
                    let bar = inset(&r, 3);
                    inside.fill_rect(
                        &Rect::new(inner.x + caret_x - scroll, bar.y, 1, bar.h),
                        t.primary,
                    );
                }
            }
            Kind::Area(area) => {
                if focused {
                    c.rounded(&outset(&r, t.ring), t.card_radius + t.ring, t.focus);
                }
                c.rounded(&r, t.card_radius, t.content);
                let mono = t.style(Style::Mono);
                let inner = self.field_inner(id);
                let mut view = c.clipped(&inner);
                let top = r.y + t.control_gap - area.scroll.offset;
                let first = (area.scroll.offset / mono.leading) as usize;
                let count = (r.h as i32 / mono.leading) as usize + 2;
                let (from, to) = area.edit.selection();
                let caret = area.edit.caret();
                let tint = if focused {
                    t.text_selection
                } else {
                    t.quiet_selection
                };
                for i in first..(first + count).min(area.lines.len()) {
                    let y = top + i as i32 * mono.leading;
                    let (start, end) = area.span(i);
                    let line = &area.edit.text()[start..end];
                    let x0 = inner.x - area.scroll_x;
                    let (mut sel_from, mut sel_to, mut caret_x) = (0, 0, None);
                    let source = &*text;
                    source.offsets(mono.face, mono.size, line, |offset, x| {
                        let at = start + offset;
                        if at == from.clamp(start, end) {
                            sel_from = x;
                        }
                        if at == to.clamp(start, end) {
                            sel_to = x;
                        }
                        if at == caret {
                            caret_x = Some(x);
                        }
                    });
                    // A selected line break shows as a little more selection.
                    let through = to > end && from <= end;
                    let extra = if through { mono.size as i32 / 2 } else { 0 };
                    if from != to && (sel_to + extra) > sel_from {
                        let band = Rect::new(
                            x0 + sel_from,
                            y,
                            (sel_to + extra - sel_from) as u32,
                            mono.leading as u32,
                        );
                        view.fill_rect(&band, tint);
                    }
                    let shown = shown_line(line);
                    let at = (x0, baseline_in(text, mono, y, mono.leading));
                    text.draw(&mut view, (mono.face, mono.size), &shown, at, t.primary);
                    if let (Some(x), true) = (caret_x, focused && from == to) {
                        let bar = Rect::new(x0 + x, y, 1, mono.leading as u32);
                        view.fill_rect(&bar, t.primary);
                    }
                }
                let mut whole = c.clipped(&r);
                if let Some(thumb) = area.scroll.thumb(&r, t) {
                    whole.rounded(&thumb, t.thumb_w / 2, t.thumb);
                }
            }
            Kind::List {
                rows,
                selected,
                scroll,
            } => {
                c.rounded(&r, t.card_radius, t.content);
                let mut view = c.clipped(&r);
                let ty = t.style(Style::Body);
                let row_h = t.row_h;
                let first = (scroll.offset / row_h) as usize;
                let count = (r.h as i32 / row_h) as usize + 2;
                for (i, row) in rows.iter().enumerate().skip(first).take(count) {
                    let y = r.y + i as i32 * row_h - scroll.offset;
                    let ink = if *selected == Some(i) {
                        let tint = if focused { t.accent } else { t.quiet_selection };
                        let band =
                            Rect::new(r.x + 4, y, (r.w as i32 - 8).max(0) as u32, row_h as u32);
                        view.rounded(&band, t.radius, tint);
                        if focused {
                            t.on_accent
                        } else {
                            t.primary
                        }
                    } else {
                        t.primary
                    };
                    let baseline = baseline_in(text, ty, y, row_h);
                    let at = (r.x + t.control_pad, baseline);
                    // After a tab: a second column, against the right edge.
                    let (name, detail) = row.split_once('\t').unwrap_or((row, ""));
                    text.draw(&mut view, (ty.face, ty.size), name, at, ink);
                    if !detail.is_empty() {
                        let width = text.width(ty.face, ty.size, detail);
                        let at = (r.right() - t.margin - width, baseline);
                        text.draw(&mut view, (ty.face, ty.size), detail, at, ink);
                    }
                }
                if let Some(thumb) = scroll.thumb(&r, t) {
                    view.rounded(&thumb, t.thumb_w / 2, t.thumb);
                }
            }
        }
    }
}

/// Where a line's baseline goes, its text centred in `height` from `top`.
fn baseline_in(text: &Typesetter, ty: Type, top: i32, height: i32) -> i32 {
    let m = text.metrics(ty.face, ty.size);
    top + (height - (m.ascent + m.descent)) / 2 + m.ascent
}

#[cfg(test)]
mod tests;
