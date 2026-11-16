use super::*;
use crate::{
    history::Entry,
    testing::{render, Harness, TestCtx},
};

fn editor(text: &str) -> EditorPanel {
    let mut editor = EditorPanel::new(Dialect::Postgres);
    editor.set_text(text);
    editor
}

fn press(editor: &mut EditorPanel, harness: &mut Harness, actions: &[Action]) {
    for action in actions {
        editor.handle(*action, &mut harness.ctx());
    }
}

fn typed(text: &str) -> Vec<Action> {
    text.chars()
        .map(|c| {
            if c == '\n' {
                Action::Confirm
            } else {
                Action::Input(c)
            }
        })
        .collect()
}

#[test]
fn keys_edit_at_the_cursor() {
    let mut harness = Harness::new();
    let mut e = editor("SELECT 1\nFROM t");
    press(
        &mut e,
        &mut harness,
        &[Action::Up, Action::End, Action::Left],
    );
    press(&mut e, &mut harness, &typed("0"));
    assert_eq!(e.text(), "SELECT 01\nFROM t");
    press(
        &mut e,
        &mut harness,
        &[Action::Delete, Action::Backspace, Action::Undo],
    );
    assert_eq!(e.text(), "SELECT 01\nFROM t");
    press(&mut e, &mut harness, &[Action::Redo]);
    assert_eq!(e.text(), "SELECT \nFROM t");
}

#[test]
fn the_statement_under_the_cursor_or_the_selection_runs() {
    let mut harness = Harness::new();
    let mut e = editor("select 1;\nselect 2;");
    assert_eq!(e.statement().as_deref(), Some("select 2;"));
    press(&mut e, &mut harness, &[Action::Up]);
    assert_eq!(e.statement().as_deref(), Some("select 1;"));
    press(
        &mut e,
        &mut harness,
        &[Action::Home, Action::SelectRight, Action::SelectRight],
    );
    assert_eq!(e.statement().as_deref(), Some("se"));
    assert_eq!(editor("  ").statement(), None);
}

#[test]
fn copy_cut_and_paste_use_the_clipboard() {
    let mut harness = Harness::new();
    let mut e = editor("select 1");
    press(&mut e, &mut harness, &[Action::Copy]);
    assert_eq!(harness.clipboard.text.as_deref(), Some("select 1"));
    press(&mut e, &mut harness, &[Action::SelectAll, Action::Cut]);
    assert_eq!(e.text(), "");
    press(&mut e, &mut harness, &[Action::Paste, Action::Paste]);
    assert_eq!(e.text(), "select 1select 1");
    assert_eq!(
        harness.toast_texts(),
        ["Copied the line", "Cut the selection"]
    );

    harness.clipboard.text = None;
    press(&mut e, &mut harness, &[Action::Paste]);
    assert!(harness.toast_texts()[2].starts_with("Nothing to paste"));
}

#[test]
fn word_motions_and_selection_keys_move_the_cursor() {
    let mut harness = Harness::new();
    let mut e = editor("select name from t");
    press(
        &mut e,
        &mut harness,
        &[Action::Home, Action::WordRight, Action::WordRight],
    );
    assert_eq!(e.buffer.cursor(), (0, 11));
    press(&mut e, &mut harness, &[Action::WordLeft, Action::SelectEnd]);
    assert_eq!(e.buffer.selected_text().as_deref(), Some("name from t"));
    press(
        &mut e,
        &mut harness,
        &[Action::SelectHome, Action::DeleteWord],
    );
    assert_eq!(e.text(), "name from t");
    press(
        &mut e,
        &mut harness,
        &[Action::SelectDown, Action::SelectUp, Action::SelectLeft],
    );
    press(&mut e, &mut harness, &[Action::PageDown, Action::PageUp]);
    assert_eq!(e.buffer.selection(), None);
}

#[test]
fn words_and_lines_select() {
    let mut harness = Harness::new();
    let mut e = editor("select name\nfrom t\nwhere x");
    e.buffer.set_cursor((0, 0), false);
    press(&mut e, &mut harness, &[Action::SelectWordRight]);
    assert_eq!(e.buffer.selected_text().as_deref(), Some("select"));
    press(&mut e, &mut harness, &[Action::End, Action::SelectWordLeft]);
    assert_eq!(e.buffer.selected_text().as_deref(), Some("name"));
    press(&mut e, &mut harness, &[Action::SelectLine]);
    assert_eq!(e.buffer.selected_text().as_deref(), Some("select name\n"));
    press(&mut e, &mut harness, &[Action::Down, Action::SelectLine]);
    assert_eq!(e.buffer.selected_text().as_deref(), Some("where x"));
    press(
        &mut e,
        &mut harness,
        &[
            Action::Home,
            Action::Up,
            Action::SelectLine,
            Action::SelectLine,
        ],
    );
    assert_eq!(e.buffer.selected_text().as_deref(), Some("from t\nwhere x"));
}

#[test]
fn history_search_replaces_the_text() {
    let mut harness = Harness::new();
    for sql in ["select * from users", "select * from orders"] {
        harness
            .history
            .push(Entry::now("local", "shop", sql))
            .expect("push");
    }
    let mut e = editor("draft");
    press(&mut e, &mut harness, &[Action::History]);
    assert!(e.is_modal());
    press(&mut e, &mut harness, &typed("usr"));
    e.paste("s");
    press(&mut e, &mut harness, &[Action::Backspace, Action::Confirm]);
    assert!(!e.is_modal());
    assert_eq!(e.text(), "select * from users");

    // Esc leaves the text alone; Undo brings back the draft.
    press(
        &mut e,
        &mut harness,
        &[Action::History, Action::Down, Action::Up, Action::Back],
    );
    assert_eq!(e.text(), "select * from users");
    press(&mut e, &mut harness, &[Action::Undo]);
    assert_eq!(e.text(), "draft");
}

#[test]
fn clicks_place_the_cursor_after_drawing() {
    let test = TestCtx::new();
    let mut e = editor("select 1\nfrom t");
    e.focused = true;
    render(&test, 40, 6, |f, area, ctx| e.render(f, area, ctx));
    // Border, padding, then a 3-column gutter.
    assert!(e.click(2 + 3 + 4, 2, false));
    assert_eq!(e.buffer.cursor(), (1, 4));
    assert!(e.click(2 + 3 + 2, 1, true));
    assert_eq!(e.buffer.selected_text().as_deref(), Some("lect 1\nfrom"));
    assert!(!e.click(0, 0, false));
    e.scroll_by(-3);
    assert_eq!(e.buffer.cursor().0, 0);
    e.scroll_by(3);
    assert_eq!(e.buffer.cursor().0, 1);
}

#[test]
fn editor_with_text() {
    let test = TestCtx::new();
    let mut e =
        editor("SELECT id, name -- who\nFROM customers\nWHERE id > 10 AND name = 'x';\nselect 2;");
    e.focused = true;
    e.buffer.set_cursor((1, 4), false);
    insta::assert_snapshot!(render(&test, 50, 7, |f, area, ctx| e.render(f, area, ctx)));
}

#[test]
fn editor_scrolled_sideways_with_tabs() {
    let test = TestCtx::new();
    let mut e = editor("select 'a very long line that does not fit in the panel' as x");
    e.focused = true;
    let tabs = [
        TabLabel {
            named: false,
            title: "Query 1".into(),
            active: false,
            running: true,
        },
        TabLabel {
            named: false,
            title: "Query 2".into(),
            active: true,
            running: false,
        },
    ];
    insta::assert_snapshot!(render(&test, 40, 4, |f, area, ctx| e
        .render_with(f, area, ctx, &tabs)));
}

#[test]
fn empty_editor_shows_a_hint() {
    let test = TestCtx::new();
    let e = EditorPanel::new(Dialect::Sqlite);
    insta::assert_snapshot!(render(&test, 70, 4, |f, area, ctx| e.render(f, area, ctx)));
}

#[test]
fn history_popup() {
    let test = TestCtx::new();
    let mut harness = Harness::new();
    harness
        .history
        .push(Entry {
            ok: false,
            ..Entry::now("local", "shop", "select *\n  from nope")
        })
        .expect("push");
    harness
        .history
        .push(Entry::now("local", "shop", "select 1"))
        .expect("push");
    let mut e = editor("");
    press(&mut e, &mut harness, &[Action::History]);
    insta::assert_snapshot!(render(&test, 60, 10, |f, area, ctx| e.render_overlay(f, area, ctx)));

    press(&mut e, &mut harness, &typed("zzz"));
    insta::assert_snapshot!(
        "history_popup_no_match",
        render(&test, 60, 8, |f, area, ctx| e.render_overlay(f, area, ctx))
    );
}

#[test]
fn control_characters_draw_as_spaces() {
    assert_eq!(clean("a\tb"), "a b");
    assert_eq!(width_of('\t'), 1);
    assert_eq!(width_of('界'), 2);
    assert_eq!(horizontal_scroll("abcdef", 2, 4, 10), 0);
    assert_eq!(horizontal_scroll("abcdefghij", 9, 0, 5), 5);
    assert_eq!(horizontal_scroll("abcdefghij", 6, 8, 5), 6);
    assert_eq!(horizontal_scroll("abcdefghij", 8, 2, 5), 4);
}

#[test]
fn double_and_triple_clicks_select_the_word_and_the_line() {
    let test = TestCtx::new();
    let mut e = editor("select name\nfrom t");
    e.focused = true;
    render(&test, 40, 6, |f, area, ctx| e.render(f, area, ctx));
    // Border, padding, then a 3-column gutter: column 9 is in `name`.
    let (x, y) = (2 + 3 + 9, 1);
    e.click(x, y, false);
    assert_eq!(e.buffer.selection(), None);
    e.click(x, y, false);
    assert_eq!(e.buffer.selected_text().as_deref(), Some("name"));
    e.click(x, y, false);
    assert_eq!(e.buffer.selected_text().as_deref(), Some("select name\n"));
    // Shift+click extends from where the cursor was.
    e.click(2 + 3, 1, false);
    e.click(2 + 3 + 4, 2, true);
    assert_eq!(
        e.buffer.selected_text().as_deref(),
        Some("select name\nfrom")
    );
}

#[test]
fn dragging_past_the_edges_scrolls() {
    let test = TestCtx::new();
    let text: Vec<String> = (0..30).map(|i| format!("select {i};")).collect();
    let mut e = editor(&text.join("\n"));
    e.focused = true;
    e.buffer.set_cursor((20, 0), false);
    render(&test, 40, 6, |f, area, ctx| e.render(f, area, ctx));
    let (top, _) = e.scroll.get();
    e.click(2 + 3, 0, true);
    assert_eq!(e.buffer.cursor().0, top - 1);
    render(&test, 40, 6, |f, area, ctx| e.render(f, area, ctx));
    assert_eq!(e.scroll.get().0, top - 1);
    e.click(2 + 3, 5, true);
    render(&test, 40, 6, |f, area, ctx| e.render(f, area, ctx));
    assert!(e.scroll.get().0 >= top);
}

#[test]
fn cut_without_a_selection_takes_the_line() {
    let mut harness = Harness::new();
    let mut e = editor("one\ntwo\nthree");
    e.buffer.set_cursor((1, 1), false);
    press(&mut e, &mut harness, &[Action::Cut]);
    assert_eq!(e.text(), "one\nthree");
    assert_eq!(harness.clipboard.text.as_deref(), Some("two\n"));
    // The last line goes with the break before it.
    press(&mut e, &mut harness, &[Action::Down, Action::Cut]);
    assert_eq!(e.text(), "one");
    assert_eq!(harness.clipboard.text.as_deref(), Some("three\n"));
    press(&mut e, &mut harness, &[Action::Cut]);
    assert_eq!(e.text(), "");
    // Nothing left to cut: the clipboard keeps the line.
    press(&mut e, &mut harness, &[Action::Cut]);
    assert_eq!(harness.clipboard.text.as_deref(), Some("one\n"));
    assert_eq!(
        harness.toast_texts(),
        ["Cut the line", "Cut the line", "Cut the line"]
    );
    press(&mut e, &mut harness, &[Action::Undo]);
    assert_eq!(e.text(), "one");
}

#[test]
fn the_signature_shows_while_typing_the_arguments() {
    let test = TestCtx::new();
    let mut e = editor("select date_trunc('day', created");
    e.language = QueryLanguage::Sql(SqlDialect::Postgres);
    e.focused = true;
    e.complete(&Catalog::default(), Action::Input('d'));
    let screen = render(&test, 50, 6, |f, area, ctx| {
        e.render(f, area, ctx);
        e.render_overlay(f, area, ctx);
    });
    assert!(screen.contains("date_trunc(field, source)"), "{screen}");
    // Past the closing parenthesis it goes away.
    e.buffer.insert_str(") ");
    e.complete(&Catalog::default(), Action::Input(' '));
    assert_eq!(e.signature, None);
}

#[test]
fn mongo_methods_show_their_arguments() {
    let mut e = editor("db.users.updateOne({ a: 1 }, ");
    e.language = QueryLanguage::MongoShell;
    e.complete(&Catalog::default(), Action::Input(' '));
    assert_eq!(
        e.signature,
        Some(Signature {
            name: "updateOne".to_string(),
            parts: vec!["filter", "update", "options"],
            current: 1,
            call: true,
        })
    );
}

#[test]
fn the_bracket_at_the_cursor_finds_its_match_outside_strings() {
    let lines: Vec<String> = ["select f(a, ')'", ", (b))"].map(String::from).to_vec();
    let highlight = sql::highlight(&lines, Dialect::Postgres);
    assert_eq!(matching(&lines, &highlight, (0, 8)), Some([(0, 8), (1, 5)]));
    assert_eq!(matching(&lines, &highlight, (1, 6)), Some([(1, 5), (0, 8)]));
    assert_eq!(matching(&lines, &highlight, (0, 13)), None);
    assert_eq!(matching(&lines, &highlight, (0, 2)), None);
}

#[test]
fn ctrl_slash_comments_in_the_editors_language() {
    let mut harness = Harness::new();
    let mut e = EditorPanel::new(Dialect::Postgres).with_language(QueryLanguage::MongoShell);
    e.set_text("db.users.find()");
    press(&mut e, &mut harness, &[Action::Comment]);
    assert_eq!(e.text(), "// db.users.find()");
}
