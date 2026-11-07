// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
extern crate std;

use super::*;
use alloc::vec;
use aricode_proto::window::KeyState;

const W: u32 = 400;
const H: u32 = 300;

/// A window, a screen of its pixels, and the widgets of a small form.
struct Form {
    ui: Ui,
    pixels: Vec<u32>,
    title: Id,
    field: Id,
    list: Id,
    ok: Id,
    cancel: Id,
}

fn form() -> Form {
    let mut ui = Ui::new(Theme::light(), W, H);
    let title = ui.label("Files", Style::Headline, Tone::Primary);
    let field = ui.field(32, "Name");
    let list = ui.list();
    ui.set_flex(list, 1);
    ui.set_rows(
        list,
        [
            "one", "two", "three", "four", "five", "six", "seven", "eight",
        ],
    );
    let cancel = ui.button("Cancel");
    let ok = ui.default_button("Open");
    let spacer = ui.spacer();
    let buttons = ui.stack(Stack::row(), &[spacer, cancel, ok]);
    let root = ui.stack(
        Stack::column().pad(Space::Margin).fill(Fill::Window),
        &[title, field, list, buttons],
    );
    ui.set_root(root);
    let mut f = Form {
        ui,
        pixels: vec![0; (W * H) as usize],
        title,
        field,
        list,
        ok,
        cancel,
    };
    f.paint();
    f
}

impl Form {
    fn paint(&mut self) -> Frame {
        let mut c = Canvas::new(&mut self.pixels, W, H, W).unwrap();
        self.ui.paint(&mut c)
    }

    fn rect(&self, id: Id) -> Rect {
        self.ui.node(id).rect
    }

    fn keys(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.ui.handle(Event::Key(b));
        }
    }

    fn click(&mut self, id: Id) {
        let r = self.rect(id);
        let (x, y) = (r.x + r.w as i32 / 2, r.y + r.h as i32 / 2);
        self.ui.handle(Event::Click { x, y, button: LEFT });
        self.ui.handle(Event::Release { x, y, button: LEFT });
    }

    fn actions(&mut self) -> Vec<Action> {
        core::iter::from_fn(|| self.ui.next_action()).collect()
    }

    fn mods(&mut self, mods: u8) {
        let state = KeyState::Pressed;
        self.ui.handle(Event::KeyCode {
            code: 0,
            state,
            mods,
        });
    }

    /// What this paint left must be what painting everything gives.
    fn same_as_full_paint(&mut self) {
        self.paint();
        let mut whole = vec![0; (W * H) as usize];
        let mut c = Canvas::new(&mut whole, W, H, W).unwrap();
        self.ui.paint_all(&mut c);
        assert!(
            self.pixels == whole,
            "a partial paint differs from a full one"
        );
    }
}

#[test]
fn stacks_follow_the_theme() {
    let f = form();
    let t = Theme::light();
    let (m, gap) = (t.margin, t.control_gap);
    assert_eq!(f.rect(f.title).x, m);
    assert_eq!(f.rect(f.title).y, m);
    let field = f.rect(f.field);
    assert_eq!(field.w as i32, W as i32 - 2 * m);
    assert_eq!(field.h as i32, t.control_h);
    assert_eq!(field.y, f.rect(f.title).bottom() + gap);
    // The list takes what is left, and the buttons sit at the bottom right.
    let ok = f.rect(f.ok);
    assert_eq!(ok.bottom(), H as i32 - m);
    assert_eq!(ok.right(), W as i32 - m);
    assert_eq!(f.rect(f.list).bottom() + gap, ok.y);
    assert_eq!(f.rect(f.cancel).right() + gap, ok.x);
}

#[test]
fn a_new_size_moves_everything_and_paints_it_all() {
    let mut f = form();
    f.ui.resize(600, 500);
    let mut big = vec![0; 600 * 500];
    let mut c = Canvas::new(&mut big, 600, 500, 600).unwrap();
    f.ui.paint_all(&mut c);
    assert_eq!(f.rect(f.ok).right(), 580);
    assert_eq!(f.rect(f.ok).bottom(), 480);
    assert_eq!(f.rect(f.field).w, 560);
    assert!(big.iter().all(|&p| p != 0), "every pixel painted");
}

#[test]
fn nothing_changed_means_nothing_to_present() {
    let mut f = form();
    assert!(f.paint().is_empty());
    let same = f.ui.text(f.title).len();
    assert_eq!(same, 5);
    f.ui.set_text(f.title, "Files");
    assert!(f.paint().is_empty(), "saying the same again costs nothing");
}

#[test]
fn a_change_marks_only_its_place() {
    let mut f = form();
    f.ui.set_tone(f.title, Tone::Error);
    let frame = f.paint();
    assert_eq!(frame.rects().len(), 1);
    let r = frame.rects()[0];
    let title = f.rect(f.title);
    assert_eq!((r.x as i32, r.y as i32), (title.x, title.y));
    assert_eq!(r.height as u32, title.h);
    f.same_as_full_paint();
}

#[test]
fn tab_walks_the_controls_and_shift_goes_back() {
    let mut f = form();
    f.keys(b"\t");
    assert_eq!(f.ui.focused(), Some(f.field));
    f.keys(b"\t");
    assert_eq!(f.ui.focused(), Some(f.list));
    f.keys(b"\t");
    assert_eq!(f.ui.focused(), Some(f.cancel));
    f.keys(b"\t");
    assert_eq!(f.ui.focused(), Some(f.ok));
    f.keys(b"\t");
    assert_eq!(f.ui.focused(), Some(f.field), "round again");
    f.mods(mods::SHIFT);
    f.keys(b"\t");
    assert_eq!(f.ui.focused(), Some(f.ok));
    f.same_as_full_paint();
}

#[test]
fn a_disabled_button_takes_no_part() {
    let mut f = form();
    f.ui.set_enabled(f.cancel, false);
    f.click(f.cancel);
    assert_eq!(f.actions(), []);
    f.ui.focus(f.list);
    f.keys(b"\t");
    assert_eq!(f.ui.focused(), Some(f.ok));
    f.same_as_full_paint();
}

#[test]
fn a_button_fires_on_release_inside_only() {
    let mut f = form();
    f.click(f.cancel);
    assert_eq!(f.actions(), [Action::Pressed(f.cancel)]);
    // Pressed, then let go elsewhere: nothing.
    let r = f.rect(f.cancel);
    let (x, y) = (r.x + 2, r.y + 2);
    f.ui.handle(Event::Click { x, y, button: LEFT });
    f.same_as_full_paint();
    f.ui.handle(Event::Motion {
        x: 0,
        y: 0,
        buttons: 1,
    });
    f.same_as_full_paint();
    f.ui.handle(Event::Release {
        x: 0,
        y: 0,
        button: LEFT,
    });
    assert_eq!(f.actions(), []);
    f.same_as_full_paint();
    // Only the left button.
    f.ui.handle(Event::Click { x, y, button: 2 });
    f.ui.handle(Event::Release { x, y, button: 2 });
    assert_eq!(f.actions(), []);
}

#[test]
fn enter_and_space_press_the_focused_button_and_enter_the_default_one() {
    let mut f = form();
    f.ui.focus(f.cancel);
    f.keys(b"\r ");
    assert_eq!(
        f.actions(),
        [Action::Pressed(f.cancel), Action::Pressed(f.cancel)]
    );
    f.ui.focus(f.title);
    f.keys(b"\r");
    assert_eq!(f.actions(), [Action::Pressed(f.ok)]);
    f.ui.set_enabled(f.ok, false);
    f.keys(b"\r");
    assert_eq!(f.actions(), []);
}

#[test]
fn a_field_edits_utf8_with_a_selection() {
    let mut f = form();
    f.click(f.field);
    assert_eq!(f.ui.focused(), Some(f.field));
    f.keys("año€".as_bytes());
    assert_eq!(f.ui.text(f.field), "año€");
    assert_eq!(f.actions().len(), 4);
    f.same_as_full_paint();
    // Shift+Left twice takes "o€", and typing replaces it.
    f.mods(mods::SHIFT);
    f.keys(b"\x1b[D\x1b[D");
    f.same_as_full_paint();
    f.mods(0);
    f.keys(b"x");
    assert_eq!(f.ui.text(f.field), "añx");
    // Ctrl+A, Ctrl+C gives the program the text, End, Ctrl+V asks for it and it goes in.
    f.actions();
    f.keys(&[1, 3]);
    assert_eq!(f.actions(), [Action::Copied]);
    let copied = f.ui.take_copied().unwrap();
    assert_eq!(copied, "añx");
    assert_eq!(f.ui.take_copied(), None);
    f.keys(b"\x1b[F");
    f.keys(&[0x16]);
    assert_eq!(f.actions(), [Action::Paste(f.field)]);
    assert_eq!(
        f.ui.text(f.field),
        "añx",
        "nothing until the program answers"
    );
    f.ui.paste(f.field, &copied);
    assert_eq!(f.actions(), [Action::Edited(f.field)]);
    assert_eq!(f.ui.text(f.field), "añxañx");
    // Ctrl+A, Ctrl+X empties it.
    f.actions();
    f.keys(&[1, 0x18]);
    assert_eq!(f.actions(), [Action::Cut(f.field)]);
    assert_eq!(f.ui.take_copied().as_deref(), Some("añxañx"));
    assert_eq!(
        f.ui.text(f.field),
        "añxañx",
        "kept until the clipboard has it"
    );
    f.ui.cut_done(f.field);
    assert_eq!(f.ui.text(f.field), "");
    f.same_as_full_paint();
    f.actions();
    f.keys(b"\r");
    assert_eq!(f.actions(), [Action::Submitted(f.field)]);
}

#[test]
fn a_long_field_keeps_its_caret_in_view() {
    let mut f = form();
    let mut ui = Ui::new(Theme::light(), W, H);
    let field = ui.field(400, "");
    ui.set_root(field);
    ui.focus(field);
    f.ui = ui;
    f.paint();
    for _ in 0..200 {
        f.ui.handle(Event::Key(b'm'));
    }
    let Kind::Field { scroll, .. } = f.ui.node(field).kind else {
        panic!()
    };
    assert!(scroll > 0);
    f.same_as_full_paint();
    f.keys(b"\x1b[H");
    let Kind::Field { scroll, .. } = f.ui.node(field).kind else {
        panic!()
    };
    assert_eq!(scroll, 0);
    f.same_as_full_paint();
}

#[test]
fn a_click_and_a_drag_select_in_a_field() {
    let mut f = form();
    f.ui.set_text(f.field, "hello world");
    let r = f.rect(f.field);
    let y = r.y + 4;
    let x = r.x + Theme::light().control_pad;
    f.ui.handle(Event::Click { x, y, button: LEFT });
    let Kind::Field { edit, .. } = &f.ui.node(f.field).kind else {
        panic!()
    };
    assert_eq!(edit.caret(), 0);
    f.ui.handle(Event::Motion {
        x: x + 500,
        y,
        buttons: 1,
    });
    f.same_as_full_paint();
    f.ui.handle(Event::Release {
        x: x + 500,
        y,
        button: LEFT,
    });
    let Kind::Field { edit, .. } = &f.ui.node(f.field).kind else {
        panic!()
    };
    assert_eq!(edit.selected(), "hello world");
    // Letting go ended it.
    f.ui.handle(Event::Motion { x, y, buttons: 0 });
    let Kind::Field { edit, .. } = &f.ui.node(f.field).kind else {
        panic!()
    };
    assert_eq!(edit.selected(), "hello world");
}

#[test]
fn a_list_takes_arrows_clicks_and_the_wheel() {
    let mut f = form();
    f.ui.focus(f.list);
    assert_eq!(f.ui.selected(f.list), Some(0));
    f.keys(b"\x1b[B");
    assert_eq!(f.actions(), [Action::Selected(f.list, 1)]);
    f.same_as_full_paint();
    f.keys(b"\r");
    assert_eq!(f.actions(), [Action::Activated(f.list, 1)]);
    // Past the end, and scrolled so it shows.
    f.keys(b"\x1b[F");
    assert_eq!(f.ui.selected(f.list), Some(7));
    f.actions();
    let view = f.rect(f.list);
    let Kind::List { scroll, .. } = f.ui.node(f.list).kind else {
        panic!()
    };
    let row_h = Theme::light().row_h;
    assert_eq!(scroll.offset, 8 * row_h - view.h as i32);
    f.same_as_full_paint();
    // A click selects, and a second one on the same row opens it.
    let (x, y) = (view.x + 30, view.bottom() - 5);
    f.ui.handle(Event::Click { x, y, button: LEFT });
    assert_eq!(f.actions(), [Action::Activated(f.list, 7)]);
    let y = view.bottom() - 5 - row_h;
    f.ui.handle(Event::Click { x, y, button: LEFT });
    assert_eq!(f.actions(), [Action::Selected(f.list, 6)]);
    f.same_as_full_paint();
    // The wheel moves the view, not the selection, and stops at the ends.
    f.ui.handle(Event::Wheel { x, y, delta: 100 });
    let Kind::List { scroll, .. } = f.ui.node(f.list).kind else {
        panic!()
    };
    assert_eq!(scroll.offset, 0);
    assert_eq!(f.ui.selected(f.list), Some(6));
    f.same_as_full_paint();
}

#[test]
fn a_list_shrinks_and_keeps_its_selection_inside() {
    let mut f = form();
    f.ui.focus(f.list);
    f.keys(b"\x1b[F");
    f.ui.set_rows(f.list, ["only"]);
    assert_eq!(f.ui.selected(f.list), Some(0));
    f.same_as_full_paint();
    f.ui.set_rows(f.list, core::iter::empty());
    assert_eq!(f.ui.selected(f.list), None);
    f.actions();
    f.keys(b"\x1b[B\r");
    assert_eq!(
        f.actions(),
        [Action::Pressed(f.ok)],
        "Enter goes on to the default"
    );
    f.same_as_full_paint();
}

#[test]
fn the_thumb_drags_the_view() {
    let mut f = form();
    let view = f.rect(f.list);
    let thumb = {
        let Kind::List { scroll, .. } = f.ui.node(f.list).kind else {
            panic!()
        };
        scroll.thumb(&view, &Theme::light()).unwrap()
    };
    let x = thumb.x + 1;
    f.ui.handle(Event::Click {
        x,
        y: thumb.y + 1,
        button: LEFT,
    });
    assert_eq!(f.actions(), [], "a thumb is not a row");
    f.ui.handle(Event::Motion {
        x,
        y: view.bottom() + 50,
        buttons: 1,
    });
    f.same_as_full_paint();
    f.ui.handle(Event::Release {
        x,
        y: view.bottom() + 50,
        button: LEFT,
    });
    let Kind::List { scroll, .. } = f.ui.node(f.list).kind else {
        panic!()
    };
    assert_eq!(scroll.offset, scroll.furthest(view.h as i32));
}

/// A tall form in a scroll view.
fn tall() -> (Ui, Id, Vec<Id>) {
    let mut ui = Ui::new(Theme::light(), W, H);
    let buttons: Vec<Id> = (0..20).map(|_| ui.button("Row")).collect();
    let column = ui.stack(Stack::column(), &buttons);
    let view = ui.scroll(column);
    ui.set_flex(view, 1);
    let root = ui.stack(
        Stack::column().pad(Space::Margin).fill(Fill::Window),
        &[view],
    );
    ui.set_root(root);
    (ui, view, buttons)
}

fn tall_form() -> (Form, Id, Vec<Id>) {
    let mut f = form();
    let (ui, view, buttons) = tall();
    f.ui = ui;
    f.same_as_full_paint();
    (f, view, buttons)
}

#[test]
fn a_scroll_view_clips_moves_and_follows_the_focus() {
    let (mut f, view, buttons) = tall_form();
    let area = f.rect(view);
    let last = buttons[19];
    assert!(f.rect(last).y > area.bottom());
    // What is outside the view cannot be clicked through it.
    let below = area.bottom() + 5;
    assert_eq!(f.ui.root.and_then(|r| f.ui.hit(r, area.x + 5, below)), None);
    // Tab to the last button scrolls it in.
    f.ui.focus(last);
    f.paint();
    assert_eq!(f.rect(last).bottom(), area.bottom());
    f.same_as_full_paint();
    f.ui.handle(Event::Wheel {
        x: area.x + 5,
        y: area.y + 5,
        delta: 1000,
    });
    f.paint();
    assert_eq!(f.rect(buttons[0]).y, area.y);
    f.same_as_full_paint();
}

#[test]
fn a_paragraph_breaks_at_spaces_and_a_word_too_long_is_cut() {
    let mut ui = Ui::new(Theme::light(), 200, 200);
    let p = ui.paragraph(
        "The quick brown fox jumps over the lazy dog, again and again",
        Style::Body,
        Tone::Primary,
    );
    ui.set_root(p);
    let mut pixels = vec![0; 200 * 200];
    let mut c = Canvas::new(&mut pixels, 200, 200, 200).unwrap();
    ui.paint_all(&mut c);
    let Kind::Label { text, lines, .. } = &ui.node(p).kind else {
        panic!()
    };
    assert!(lines.len() > 1);
    let ty = Theme::light().style(Style::Body);
    let mut joined = String::new();
    for &(from, to) in lines {
        let line = &text[from as usize..to as usize];
        assert!(!line.starts_with(' ') && !line.ends_with(' '), "{line:?}");
        assert!(ui.text.width(ty.face, ty.size, line) <= 200, "{line:?}");
        joined.push_str(line);
        joined.push(' ');
    }
    assert_eq!(joined.trim_end(), text);

    let long = "w".repeat(100);
    let lines = ui.wrapped(&long, ty, 200);
    assert!(lines.len() > 1);
    for &(from, to) in &lines {
        assert!(
            ui.text
                .width(ty.face, ty.size, &long[from as usize..to as usize])
                <= 200
        );
    }
    assert_eq!(lines.last().unwrap().1 as usize, long.len());

    let lines = ui.wrapped("one\n\ntwo", ty, 200);
    assert_eq!(lines, [(0, 3), (4, 4), (5, 8)]);
}

#[test]
fn a_hidden_widget_gives_back_its_room_and_the_keys() {
    let mut f = form();
    f.ui.focus(f.field);
    let list_y = f.rect(f.list).y;
    f.ui.set_hidden(f.field, true);
    assert_eq!(f.ui.focused(), None);
    f.same_as_full_paint();
    assert!(f.rect(f.list).y < list_y);
    f.ui.set_hidden(f.field, false);
    f.same_as_full_paint();
    assert_eq!(f.rect(f.list).y, list_y);
}

#[test]
fn the_theme_changes_every_pixel_it_owns() {
    let mut f = form();
    let corner = f.pixels[0];
    f.ui.handle(Event::Appearance { dark: true });
    f.same_as_full_paint();
    assert_ne!(f.pixels[0], corner);
    assert_eq!(f.pixels[0], Theme::dark().window);
}

#[test]
fn losing_the_keys_quiets_the_focus() {
    let mut f = form();
    f.ui.focus(f.list);
    f.same_as_full_paint();
    let before = f.pixels.clone();
    f.ui.handle(Event::Focus(false));
    f.same_as_full_paint();
    assert_ne!(f.pixels, before, "the selection is no longer the accent");
    f.ui.handle(Event::Focus(true));
    f.same_as_full_paint();
    assert_eq!(f.pixels, before);
}

#[test]
fn escape_and_shortcuts_go_to_the_program() {
    let mut f = form();
    f.ui.focus(f.list);
    f.keys(&[0x13]);
    assert_eq!(f.actions(), [Action::Shortcut(b's')]);
    // A lone Escape is told when the next byte shows it was not a sequence.
    f.keys(&[0x1b, 0x1b]);
    assert_eq!(f.actions(), [Action::Cancel]);
}

/// A window that is one area.
fn writer() -> (Form, Id) {
    let mut f = form();
    let mut ui = Ui::new(Theme::light(), W, H);
    let area = ui.area(64 * 1024);
    ui.set_root(area);
    ui.focus(area);
    f.ui = ui;
    f.same_as_full_paint();
    (f, area)
}

fn caret(f: &Form, area: Id) -> usize {
    let Kind::Area(a) = &f.ui.node(area).kind else {
        panic!()
    };
    a.edit.caret()
}

#[test]
fn an_area_writes_lines_tabs_and_keeps_its_line_ends() {
    let (mut f, area) = writer();
    f.keys(b"uno\rdos\ttres");
    assert_eq!(f.ui.text(area), "uno\ndos\ttres");
    f.same_as_full_paint();
    // Shift+Tab leaves it instead.
    f.mods(mods::SHIFT);
    f.keys(b"\t");
    assert_eq!(f.ui.text(area), "uno\ndos\ttres");
    f.mods(0);
    f.ui.set_text(area, "a\r\nb");
    assert_eq!(f.ui.text(area), "a\r\nb");
    assert_eq!(caret(&f, area), 0, "a new text starts at its top");
    f.same_as_full_paint();
}

#[test]
fn up_and_down_keep_their_aim_across_a_short_line() {
    let (mut f, area) = writer();
    f.ui.set_text(area, "long line\nab\nlong line");
    f.ui.focus(area);
    f.keys(b"\x1b[F");
    assert_eq!(caret(&f, area), 9);
    f.keys(b"\x1b[B");
    assert_eq!(caret(&f, area), 12, "the end of the short line");
    f.keys(b"\x1b[B");
    assert_eq!(caret(&f, area), 22, "back at the same column");
    f.keys(b"\x1b[B");
    assert_eq!(caret(&f, area), 22, "the end of the text");
    f.keys(b"\x1b[A\x1b[A\x1b[A\x1b[A");
    assert_eq!(caret(&f, area), 0, "the start of the text");
    f.same_as_full_paint();
}

#[test]
fn a_long_text_scrolls_to_its_caret_both_ways() {
    let (mut f, area) = writer();
    let mut text = String::new();
    for _ in 0..200 {
        text.push_str("line\n");
    }
    text.push_str(&"m".repeat(300));
    f.ui.set_text(area, &text);
    f.keys(b"\x1b[A");
    for _ in 0..3 {
        f.keys(b"\x1b[B");
    }
    f.same_as_full_paint();
    // To the last line, by clicks on the scroll bar's end and the keyboard.
    for _ in 0..200 {
        f.keys(b"\x1b[B");
    }
    f.keys(b"\x1b[F");
    f.same_as_full_paint();
    let Kind::Area(a) = &f.ui.node(area).kind else {
        panic!()
    };
    assert!(a.scroll.offset > 0);
    assert!(a.scroll_x > 0);
    let furthest = a.scroll.furthest(H as i32);
    assert_eq!(a.scroll.offset, furthest);
    // The wheel moves the view and not the caret.
    let at = caret(&f, area);
    f.ui.handle(Event::Wheel {
        x: 5,
        y: 5,
        delta: 1,
    });
    f.same_as_full_paint();
    assert_eq!(caret(&f, area), at);
}

#[test]
fn a_click_and_a_drag_select_lines_in_an_area() {
    let (mut f, area) = writer();
    f.ui.set_text(area, "first\nsecond\nthird");
    f.paint();
    let t = Theme::light();
    let x0 = t.control_pad;
    let row = |n: i32| t.control_gap + n * t.style(Style::Mono).leading + 2;
    f.ui.handle(Event::Click {
        x: x0,
        y: row(1),
        button: LEFT,
    });
    assert_eq!(caret(&f, area), 6);
    f.ui.handle(Event::Motion {
        x: x0 + 500,
        y: row(2),
        buttons: 1,
    });
    f.same_as_full_paint();
    f.ui.handle(Event::Release {
        x: x0 + 500,
        y: row(2),
        button: LEFT,
    });
    let Kind::Area(a) = &f.ui.node(area).kind else {
        panic!()
    };
    assert_eq!(a.edit.selected(), "second\nthird");
    f.keys(b"x");
    assert_eq!(f.ui.text(area), "first\nx");
    f.same_as_full_paint();
}

#[test]
fn delete_on_a_list_goes_to_the_program() {
    let mut f = form();
    f.ui.focus(f.list);
    f.keys(b"\x1b[3~");
    assert_eq!(f.actions(), [Action::DeleteRow(f.list, 0)]);
}

#[test]
fn a_row_has_a_second_column_after_a_tab() {
    let mut f = form();
    f.ui.set_rows(f.list, ["name\t123"]);
    f.same_as_full_paint();
    let r = f.rect(f.list);
    let t = Theme::light();
    let right_part: u32 = (r.right() - t.margin - 30..r.right() - t.margin)
        .map(|x| {
            let at = (r.y + t.row_h / 2) as usize * W as usize + x as usize;
            u32::from(f.pixels[at] != t.accent)
        })
        .sum();
    assert!(right_part > 0, "the number is drawn near the right edge");
}

#[test]
fn a_size_refused_changes_nothing() {
    let mut f = form();
    let refused: Result<(), ()> = f.ui.configure(600, 500, |_| Err(()));
    assert!(refused.is_err());
    assert_eq!(f.ui.size(), (W, H));
    f.same_as_full_paint();
    assert_eq!(f.rect(f.ok).right(), W as i32 - Theme::light().margin);
}

#[test]
fn a_held_key_edits_and_moves_but_never_answers_a_question() {
    let mut f = form();
    let code = |state| Event::KeyCode {
        code: 28,
        state,
        mods: 0,
    };
    // On a list: the arrows repeat; Enter and Delete do not.
    f.ui.focus(f.list);
    f.ui.handle(code(KeyState::Repeated));
    f.keys(b"\x1b[B");
    assert_eq!(f.ui.selected(f.list), Some(1));
    f.actions();
    f.keys(b"\r\x1b[3~");
    assert_eq!(f.actions(), [], "held, it opens and deletes nothing");
    // On a button, in a field, and for what no widget takes.
    f.ui.focus(f.cancel);
    f.keys(b"\r ");
    assert_eq!(f.actions(), []);
    f.ui.focus(f.field);
    f.keys(b"ab\r");
    assert_eq!(f.ui.text(f.field), "ab", "typing repeats");
    f.actions();
    assert_eq!(f.ui.next_action(), None);
    f.keys(&[0x16, 3, 0x18]);
    assert_eq!(f.actions(), [], "nor the clipboard");
    f.ui.focus(f.title);
    f.keys(&[b'\r', 0x13]);
    assert_eq!(f.actions(), []);
    // A new press answers again.
    f.ui.handle(code(KeyState::Pressed));
    f.keys(b"\r");
    assert_eq!(f.actions(), [Action::Pressed(f.ok)]);
}

#[test]
fn a_button_gone_while_held_does_not_fire() {
    let mut f = form();
    let r = f.rect(f.cancel);
    let (x, y) = (r.x + 2, r.y + 2);
    f.ui.handle(Event::Click { x, y, button: LEFT });
    f.ui.set_enabled(f.cancel, false);
    f.ui.handle(Event::Release { x, y, button: LEFT });
    assert_eq!(f.actions(), []);
    f.same_as_full_paint();
}

#[test]
fn a_hidden_stack_hides_its_default_button_and_its_focus_ring() {
    let mut ui = Ui::new(Theme::light(), W, H);
    let ok = ui.default_button("Open");
    let inner = ui.stack(Stack::column(), &[ok]);
    let root = ui.stack(Stack::column().pad(Space::Margin), &[inner]);
    ui.set_root(root);
    ui.focus(ok);
    let mut f = form();
    f.ui = ui;
    f.same_as_full_paint();
    f.ui.set_hidden(inner, true);
    assert_eq!(f.ui.focused(), None);
    f.same_as_full_paint();
    f.keys(b"\r");
    assert_eq!(f.actions(), []);
}

#[test]
fn a_long_line_shows_where_the_caret_is() {
    let (mut f, area) = writer();
    f.ui.set_text(area, &"m".repeat(1000));
    f.keys(b"\x1b[F");
    f.same_as_full_paint();
    // Text is drawn up to the caret, at the right of the view.
    let t = Theme::light();
    let y = (t.control_gap + 9) as usize;
    let x = W as usize - t.control_pad as usize - 4;
    let row = &f.pixels[y * W as usize..(y + 1) * W as usize];
    assert!(
        row[x - 30..x].iter().any(|&p| p != t.content),
        "glyphs near the caret"
    );
}

#[test]
fn another_widgets_text_does_not_move_a_view_the_owner_scrolled() {
    let mut f = form();
    let mut ui = Ui::new(Theme::light(), W, H);
    let status = ui.label("", Style::Callout, Tone::Primary);
    let area = ui.area(64 * 1024);
    ui.set_flex(area, 1);
    let root = ui.stack(Stack::column().pad(Space::Margin), &[status, area]);
    ui.set_root(root);
    ui.focus(area);
    f.ui = ui;
    let mut text = String::new();
    for _ in 0..100 {
        text.push_str("line\n");
    }
    f.ui.set_text(area, &text);
    f.same_as_full_paint();
    let r = f.rect(area);
    f.ui.handle(Event::Wheel {
        x: r.x + 4,
        y: r.y + 4,
        delta: -5,
    });
    f.same_as_full_paint();
    let offset = |f: &Form| {
        let Kind::Area(a) = &f.ui.node(area).kind else {
            panic!()
        };
        a.scroll.offset
    };
    let scrolled = offset(&f);
    assert!(scrolled > 0);
    f.ui.set_text(status, "saved");
    f.same_as_full_paint();
    assert_eq!(offset(&f), scrolled);
}

#[test]
fn a_paste_says_when_it_did_not_all_fit() {
    let mut ui = Ui::new(Theme::light(), W, H);
    let field = ui.field(4, "");
    ui.set_root(field);
    assert!(ui.paste(field, "ab"));
    assert!(!ui.paste(field, "cdef"));
    assert_eq!(ui.text(field), "abcd");
}

#[test]
fn the_thumb_of_a_very_long_text_does_not_overflow() {
    let view = Rect::new(0, 0, 100, 2000);
    let t = Theme::light();
    let s = Scroller {
        offset: 1_100_000,
        content: 1_200_000,
    };
    let thumb = s.thumb(&view, &t).unwrap();
    assert!(thumb.y >= 0 && thumb.bottom() <= 2000);
    let to = s.dragged_to(&view, &t, 1900, 0);
    assert!(to > 0 && to <= s.furthest(2000));
}

/// A form with a question ready.
fn asking() -> (Form, Dialog) {
    let mut f = form();
    let d = f.ui.dialog("Delete", "Cancel");
    f.same_as_full_paint();
    (f, d)
}

#[test]
fn a_dialog_takes_the_keys_and_gives_them_back() {
    let (mut f, d) = asking();
    f.ui.focus(f.list);
    f.ui.ask(&d, "Delete it?", "Delete");
    assert!(f.ui.asking());
    assert_eq!(
        f.ui.focused(),
        Some(d.no),
        "the keys start on the safe answer"
    );
    f.same_as_full_paint();
    // Tab stays inside, on its two answers.
    f.keys(b"\t");
    assert_eq!(f.ui.focused(), Some(d.yes));
    f.keys(b"\t");
    assert_eq!(f.ui.focused(), Some(d.no));
    f.same_as_full_paint();
    // Keys for the window behind go nowhere; Enter answers what has the focus.
    f.keys(b"\x1b[B");
    assert_eq!(f.ui.selected(f.list), Some(0));
    f.actions();
    f.keys(&[0x13]);
    assert_eq!(f.actions(), [], "no shortcut behind a question");
    f.keys(b"\r");
    assert_eq!(f.actions(), [Action::Pressed(d.no)]);
    f.ui.answered();
    assert!(!f.ui.asking());
    assert_eq!(f.ui.focused(), Some(f.list));
    f.same_as_full_paint();
}

#[test]
fn nothing_behind_a_dialog_can_be_clicked() {
    let (mut f, d) = asking();
    f.ui.ask(&d, "Delete it?", "Delete");
    f.paint();
    f.click(f.cancel);
    f.click(f.ok);
    assert_eq!(f.actions(), []);
    // Outside its card: nothing either, and it stays.
    f.ui.handle(Event::Click {
        x: 1,
        y: 1,
        button: LEFT,
    });
    f.ui.handle(Event::Release {
        x: 1,
        y: 1,
        button: LEFT,
    });
    assert_eq!(f.actions(), []);
    assert!(f.ui.asking());
    f.click(d.yes);
    assert_eq!(f.actions(), [Action::Pressed(d.yes)]);
}

#[test]
fn a_held_key_does_not_answer_a_dialog_and_esc_says_no() {
    let (mut f, d) = asking();
    f.ui.ask(&d, "Delete it?", "Delete");
    f.paint();
    let code = |code, state| Event::KeyCode {
        code,
        state,
        mods: 0,
    };
    f.ui.handle(code(111, KeyState::Repeated));
    f.keys(b"\r ");
    assert_eq!(f.actions(), [], "held");
    // Esc answers at once, without waiting for the key after it, and only once.
    f.ui.handle(code(1, KeyState::Pressed));
    assert_eq!(f.actions(), [Action::Pressed(d.no)]);
    f.keys(&[0x1b]);
    f.ui.handle(code(1, KeyState::Released));
    f.ui.handle(code(30, KeyState::Pressed));
    f.keys(b"a");
    assert_eq!(f.actions(), []);
}

#[test]
fn a_dialog_paints_what_a_full_paint_does_and_fits_a_small_window() {
    let (mut f, d) = asking();
    f.ui.ask(
        &d,
        "A question long enough to need more than one line in this card, surely.",
        "Do it",
    );
    f.same_as_full_paint();
    assert!(f.rect(d.yes).bottom() < H as i32);
    // What changes behind it is painted under it.
    f.ui.set_text(f.title, "Another title");
    f.same_as_full_paint();
    f.ui.answered();
    f.same_as_full_paint();
    let mut small = Ui::new(Theme::light(), 100, 60);
    let d = small.dialog("Yes", "No");
    let label = small.label("x", Style::Body, Tone::Primary);
    small.set_root(label);
    small.ask(&d, "Fits?", "Yes");
    let mut pixels = vec![0; 100 * 60];
    let mut c = Canvas::new(&mut pixels, 100, 60, 100).unwrap();
    small.paint_all(&mut c);
}

/// The `ui.*` figures of ADR 0078, on the host: `cargo test --release -p aricode-ui --
/// --ignored --nocapture ui_figures`. They compare builds on one machine, and say what
/// grew; a program on the target pays what its processor pays.
#[test]
#[ignore]
fn ui_figures() {
    use aricode_text::{Face, Typesetter, INTER_REGULAR};
    use std::time::Instant;
    const FACE: Face = Face(INTER_REGULAR);
    fn figure(name: &str, runs: u32, mut work: impl FnMut()) {
        let start = Instant::now();
        for _ in 0..runs {
            work();
        }
        let each = start.elapsed().as_nanos() as f64 / f64::from(runs) / 1000.0;
        std::println!("ui.{name}: {each:.1} us");
    }
    const SW: u32 = 1366;
    const SH: u32 = 768;
    let mut pixels = vec![0u32; (SW * SH) as usize];

    let mut canvas = Canvas::new(&mut pixels, SW, SH, SW).unwrap();
    figure("glyph_cold", 50, || {
        let mut text = Typesetter::new();
        text.draw(&mut canvas, (FACE, 13), "g", (10, 20), 0);
    });
    let mut text = Typesetter::new();
    text.draw(&mut canvas, (FACE, 13), "g", (10, 20), 0);
    figure("glyph_warm", 1000, || {
        text.draw(&mut canvas, (FACE, 13), "g", (10, 20), 0);
    });
    let line = "The quick brown fox jumps over the lazy dog, again and again";
    figure("line", 1000, || {
        text.draw(&mut canvas, (FACE, 13), line, (10, 40), 0);
    });
    let frame = Rect::new(100, 100, 720, 400);
    figure("shadow", 200, || canvas.window_shadow(&frame, 77));
    let button = Rect::new(100, 100, 80, 22);
    figure("rounded_control", 1000, || canvas.rounded(&button, 6, 0));

    let mut ui = Ui::new(Theme::light(), SW, SH);
    let title = ui.label("Files", Style::Headline, Tone::Primary);
    let field = ui.field(64, "Name");
    let list = ui.list();
    ui.set_flex(list, 1);
    let rows: Vec<String> = (0..64).map(|i| std::format!("file-{i}\t{i}")).collect();
    ui.set_rows(list, rows.iter().map(String::as_str));
    let spacer = ui.spacer();
    let cancel = ui.button("Cancel");
    let ok = ui.default_button("Open");
    let buttons = ui.stack(Stack::row(), &[spacer, cancel, ok]);
    let root = ui.stack(
        Stack::column().pad(Space::Margin).fill(Fill::Window),
        &[title, field, list, buttons],
    );
    ui.set_root(root);
    ui.focus(field);
    ui.paint_all(&mut canvas);
    figure("layout", 200, || {
        ui.layout();
        ui.damage.clear();
    });
    figure("full_repaint", 50, || ui.paint_all(&mut canvas));
    figure("button_press", 500, || {
        ui.handle(Event::Click {
            x: 0,
            y: 0,
            button: LEFT,
        });
        ui.dirty(ok);
        ui.paint(&mut canvas);
    });
    figure("key_in_a_field", 200, || {
        ui.handle(Event::Key(b'a'));
        ui.handle(Event::Key(0x7F));
        ui.paint(&mut canvas);
    });
    let area = ui.area(64 * 1024);
    ui.set_root(area);
    ui.focus(area);
    let text_64k = "A line of text for the editor.\n".repeat(2100);
    ui.set_text(area, &text_64k);
    ui.paint_all(&mut canvas);
    figure("key_in_64k_of_text", 200, || {
        ui.handle(Event::Key(b'a'));
        ui.paint(&mut canvas);
        ui.handle(Event::Key(0x7F));
        ui.paint(&mut canvas);
    });
}

#[test]
fn a_field_hands_its_arrows_to_the_list_it_searches() {
    let mut f = form();
    f.ui.focus(f.field);
    f.keys(b"\x1b[B\x1b[A");
    assert_eq!(
        f.actions(),
        [Action::Arrow(f.field, false), Action::Arrow(f.field, true)]
    );
    f.ui.select_next(f.list, false);
    assert_eq!(f.ui.selected(f.list), Some(1));
    f.ui.select_next(f.list, true);
    f.ui.select_next(f.list, true);
    assert_eq!(f.ui.selected(f.list), Some(0), "it stops at the top");
    for _ in 0..20 {
        f.ui.select_next(f.list, false);
    }
    assert_eq!(f.ui.selected(f.list), Some(7), "and at the end");
    f.same_as_full_paint();
}

#[test]
fn the_edit_menu_does_what_its_keys_do() {
    let mut f = form();
    f.ui.focus(f.field);
    f.keys(b"abc");
    f.actions();
    let menu = |f: &mut Form, row: usize| {
        let id = EDIT_MENU[row].1;
        f.ui.handle(Event::Menu { id });
    };
    menu(&mut f, 4); // Select All
    menu(&mut f, 2); // Copy
    assert_eq!(f.actions(), [Action::Copied]);
    assert_eq!(f.ui.take_copied().as_deref(), Some("abc"));
    menu(&mut f, 1); // Cut
    assert_eq!(f.actions(), [Action::Cut(f.field)]);
    menu(&mut f, 3); // Paste
    assert_eq!(f.actions(), [Action::Paste(f.field)]);
    // A number of the program's goes to the program.
    f.ui.handle(Event::Menu { id: 7 });
    assert_eq!(f.actions(), [Action::Menu(7)]);
    // Behind a question, nothing.
    let d = f.ui.dialog("Yes", "No");
    f.ui.ask(&d, "Sure?", "Yes");
    f.ui.handle(Event::Menu { id: 7 });
    menu(&mut f, 2);
    assert_eq!(f.actions(), []);
}
