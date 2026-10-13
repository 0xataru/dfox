use super::*;

fn buffer(text: &str) -> Buffer {
    let mut buffer = Buffer::default();
    buffer.set_text(text);
    buffer
}

fn typed(buffer: &mut Buffer, text: &str) {
    for c in text.chars() {
        if c == '\n' {
            buffer.newline();
        } else {
            buffer.insert_char(c);
        }
    }
}

#[test]
fn edits_happen_at_the_cursor_not_the_end() {
    // todo #8: deleting in the middle of a query removed the last character.
    let mut b = buffer("SELECT * FROM users");
    b.set_cursor((0, 8), false);
    b.backspace();
    typed(&mut b, "id");
    assert_eq!(b.text(), "SELECT id FROM users");
    b.delete();
    assert_eq!(b.text(), "SELECT idFROM users");
}

#[test]
fn backspace_and_delete_join_lines() {
    let mut b = buffer("ab\ncd");
    b.set_cursor((1, 0), false);
    b.backspace();
    assert_eq!((b.text().as_str(), b.cursor()), ("abcd", (0, 2)));
    b.set_cursor((0, 4), false);
    b.delete();
    assert_eq!(b.text(), "abcd");
    let mut b = buffer("ab\ncd");
    b.set_cursor((0, 2), false);
    b.delete();
    assert_eq!(b.text(), "abcd");
}

#[test]
fn wide_characters_are_one_column() {
    let mut b = buffer("é😀x");
    b.set_cursor((0, 2), false);
    b.backspace();
    assert_eq!(b.text(), "éx");
    assert_eq!(b.current_char(), Some('x'));
}

#[test]
fn newline_keeps_the_indentation() {
    let mut b = buffer("select\n  a,");
    typed(&mut b, "\nb");
    assert_eq!(b.text(), "select\n  a,\n  b");
}

#[test]
fn typing_replaces_the_selection() {
    let mut b = buffer("select 1 from t");
    b.set_cursor((0, 7), false);
    b.set_cursor((0, 8), true);
    assert_eq!(b.selected_text().as_deref(), Some("1"));
    typed(&mut b, "*");
    assert_eq!(b.text(), "select * from t");
}

#[test]
fn selections_span_lines_either_way() {
    let mut b = buffer("one\ntwo\nthree");
    b.set_cursor((2, 2), false);
    b.move_cursor(Motion::Up, true);
    b.move_cursor(Motion::Up, true);
    assert_eq!(b.selected_text().as_deref(), Some("e\ntwo\nth"));
    b.move_cursor(Motion::Up, true);
    assert_eq!(b.selected_text().as_deref(), Some("one\ntwo\nth"));
    assert_eq!(b.cut().as_deref(), Some("one\ntwo\nth"));
    assert_eq!(b.text(), "ree");
    b.select_all();
    assert_eq!(b.selected_text().as_deref(), Some("ree"));
}

#[test]
fn arrows_collapse_a_selection() {
    let mut b = buffer("abcdef");
    b.set_cursor((0, 1), false);
    b.set_cursor((0, 4), true);
    b.move_cursor(Motion::Left, false);
    assert_eq!((b.cursor(), b.selection()), ((0, 1), None));
    b.set_cursor((0, 4), true);
    b.move_cursor(Motion::Right, false);
    assert_eq!(b.cursor(), (0, 4));
}

#[test]
fn up_and_down_remember_the_column() {
    let mut b = buffer("long line\nab\nlong line");
    b.set_cursor((0, 7), false);
    b.move_cursor(Motion::Down, false);
    assert_eq!(b.cursor(), (1, 2));
    b.move_cursor(Motion::Down, false);
    assert_eq!(b.cursor(), (2, 7));
    b.move_cursor(Motion::Down, false);
    assert_eq!(b.cursor(), (2, 9));
    b.move_cursor(Motion::PageUp(10), false);
    assert_eq!(b.cursor(), (0, 9));
    b.move_cursor(Motion::Up, false);
    assert_eq!(b.cursor(), (0, 0));
    b.move_cursor(Motion::PageDown(1), false);
    assert_eq!(b.cursor(), (1, 0));
}

#[test]
fn home_toggles_between_indent_and_line_start() {
    let mut b = buffer("    select");
    b.move_cursor(Motion::Home, false);
    assert_eq!(b.cursor(), (0, 4));
    b.move_cursor(Motion::Home, false);
    assert_eq!(b.cursor(), (0, 0));
    b.move_cursor(Motion::End, false);
    assert_eq!(b.cursor(), (0, 10));
}

#[test]
fn word_motions_skip_spaces_and_cross_lines() {
    let mut b = buffer("select a.id,  b\nfrom");
    b.set_cursor((0, 0), false);
    let mut stops = Vec::new();
    for _ in 0..7 {
        b.move_cursor(Motion::WordRight, false);
        stops.push(b.cursor());
    }
    assert_eq!(
        stops,
        [(0, 6), (0, 8), (0, 9), (0, 11), (0, 12), (0, 15), (1, 0)]
    );
    b.move_cursor(Motion::WordLeft, false);
    assert_eq!(b.cursor(), (0, 15));
    b.move_cursor(Motion::WordLeft, false);
    assert_eq!(b.cursor(), (0, 14));
    b.set_cursor((1, 4), false);
    b.move_cursor(Motion::WordRight, false);
    assert_eq!(b.cursor(), (1, 4));
}

#[test]
fn delete_word_removes_back_to_the_word_start() {
    let mut b = buffer("select name");
    b.delete_word();
    assert_eq!(b.text(), "select ");
    b.delete_word();
    assert_eq!(b.text(), "");
}

#[test]
fn paste_inserts_lines_in_the_middle() {
    let mut b = buffer("select  from t");
    b.set_cursor((0, 7), false);
    b.insert_str("a,\r\n  b");
    assert_eq!(b.text(), "select a,\n  b from t");
    assert_eq!(b.cursor(), (1, 3));
    b.insert_str("c");
    assert_eq!(b.text(), "select a,\n  bc from t");
}

#[test]
fn typing_undoes_by_word_and_redo_restores() {
    let mut b = Buffer::default();
    typed(&mut b, "select name");
    assert!(b.undo());
    assert_eq!(b.text(), "select ");
    assert!(b.undo());
    assert_eq!(b.text(), "select");
    assert!(b.undo());
    assert_eq!(b.text(), "");
    assert!(!b.undo());
    assert!(b.redo());
    assert!(b.redo());
    assert_eq!(b.text(), "select ");
    // A new edit drops what could be redone.
    typed(&mut b, "x");
    assert!(!b.redo());
}

#[test]
fn moving_the_cursor_can_break_an_undo_group() {
    let mut b = Buffer::default();
    typed(&mut b, "ab");
    b.break_undo();
    typed(&mut b, "cd");
    b.undo();
    assert_eq!(b.text(), "ab");
}

#[test]
fn undo_history_is_bounded() {
    let mut b = Buffer::default();
    for _ in 0..MAX_UNDO + 50 {
        b.insert_char(' ');
    }
    let mut undone = 0;
    while b.undo() {
        undone += 1;
    }
    assert_eq!(undone, MAX_UNDO);
}

#[test]
fn every_edit_changes_the_version() {
    let mut b = Buffer::default();
    let before = b.version();
    b.insert_char('x');
    assert!(b.version() > before);
}
