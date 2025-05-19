mod doc;
mod engine;
mod layout;
mod ui;

use anyhow::{anyhow, Result};
use ratatui::{
    crossterm::{
        cursor::SetCursorStyle,
        event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
        execute,
    },
    DefaultTerminal,
};
use std::env;
use std::fs;
use std::io::{self, Write};

use crate::doc::Doc;
use crate::engine::Engine;
use crate::ui::{popup_index_for_key, render, ViewInfo};

mod licenses {
    pub const ZIPIN: &str = include_str!("../../../LICENSE");
    pub const THIRD_PARTY: &str = include_str!("../../../LICENSE-THIRD-PARTY.md");
    pub const NOTICE: &str = include_str!("../../../NOTICE");
}

fn run(terminal: &mut DefaultTerminal, mut engine: Engine) -> Result<()> {
    let mut doc = Doc::new();
    let mut info = ViewInfo::default();
    let mut clipboard = arboard::Clipboard::new()?;
    apply_cursor_style(engine.ascii_mode())?;

    loop {
        terminal.draw(|frame| render(frame, &doc, &engine, &mut info))?;
        if let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = event::read()?
        {
            match (code, modifiers) {
                (KeyCode::Char('q'), KeyModifiers::CONTROL) => break,
                (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
                    doc.clear();
                    engine.reset();
                }
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                    let mut out = String::new();
                    for (i, paragraph) in doc.paragraphs.iter().enumerate() {
                        if i > 0 {
                            out.push('\n');
                        }
                        for seg in paragraph {
                            out.push_str(seg.text());
                        }
                    }
                    let trimmed = out.trim_end_matches('\n').to_string();
                    clipboard.set_text(trimmed)?;
                }
                (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
                    paste_clipboard(&mut doc, &mut clipboard)
                }
                (KeyCode::Char(' '), KeyModifiers::NONE) => {
                    if engine.is_composing() {
                        flush_composition(&mut doc, &mut engine);
                    } else if engine.ascii_mode() {
                        doc.insert_committed(" ".to_string());
                    } else {
                        doc.insert_committed("\u{3000}".to_string());
                    }
                }
                (KeyCode::Char(' '), KeyModifiers::CONTROL) => {
                    engine.toggle_ascii();
                    doc.cancel_composition();
                    apply_cursor_style(engine.ascii_mode())?;
                }
                (KeyCode::Enter, KeyModifiers::NONE) => {
                    flush_composition(&mut doc, &mut engine);
                    doc.break_line();
                }
                (KeyCode::Esc, _) => {
                    if engine.is_composing() {
                        engine.reset();
                        doc.cancel_composition();
                    }
                }
                (KeyCode::Backspace, KeyModifiers::NONE) => {
                    if engine.is_composing() {
                        engine.shrink();
                        doc.shrink_composition();
                        sync_after_engine(&mut doc, &mut engine);
                    } else {
                        doc.delete_segment_left();
                    }
                }
                (KeyCode::Delete, KeyModifiers::NONE) => doc.delete_segment_under(),
                (KeyCode::Tab, KeyModifiers::NONE) => engine.next_candidate(),
                (KeyCode::BackTab, KeyModifiers::SHIFT) => engine.prev_candidate(),
                (KeyCode::BackTab, _) => engine.prev_candidate(),
                (KeyCode::Left, KeyModifiers::NONE) => {
                    if engine.is_composing() {
                        engine.prev_page();
                    } else {
                        doc.move_cursor_left();
                    }
                }
                (KeyCode::Right, KeyModifiers::NONE) => {
                    if engine.is_composing() {
                        engine.next_page();
                    } else {
                        doc.move_cursor_right();
                    }
                }
                (KeyCode::Home, KeyModifiers::NONE) => doc.move_cursor_home(),
                (KeyCode::End, KeyModifiers::NONE) => doc.move_cursor_end(),
                (KeyCode::Up, KeyModifiers::NONE) => {
                    if engine.is_composing() {
                        engine.prev_candidate();
                    } else {
                        move_cursor_up(&mut doc, &info);
                    }
                }
                (KeyCode::Down, KeyModifiers::NONE) => {
                    if engine.is_composing() {
                        engine.next_candidate();
                    } else {
                        move_cursor_down(&mut doc, &info);
                    }
                }
                (KeyCode::Up, KeyModifiers::CONTROL) => {
                    doc.scroll_offset = doc.scroll_offset.saturating_sub(1);
                }
                (KeyCode::Down, KeyModifiers::CONTROL) => {
                    let max = info.total_rows.saturating_sub(1);
                    doc.scroll_offset = max.min(doc.scroll_offset + 1);
                }
                (KeyCode::Char(c), KeyModifiers::NONE) if c.is_ascii_digit() => {
                    if engine.is_composing() {
                        if let Some(idx) = popup_index_for_key(c) {
                            if let Some(text) = engine.commit_index_on_page(idx) {
                                doc.commit_composition(text);
                            }
                        }
                    } else {
                        doc.insert_committed(c.to_string());
                    }
                }
                (KeyCode::Char(c), KeyModifiers::SHIFT) if c.is_ascii_alphabetic() => {
                    flush_composition(&mut doc, &mut engine);
                    doc.insert_committed(c.to_ascii_uppercase().to_string());
                }
                (KeyCode::Char(c), KeyModifiers::NONE) if is_ascii_punct(c) => {
                    flush_composition(&mut doc, &mut engine);
                    let mapped = if engine.ascii_mode() {
                        c.to_string()
                    } else {
                        full_width_punct(c).to_string()
                    };
                    doc.insert_committed(mapped);
                }
                (KeyCode::Char(c), KeyModifiers::NONE) if c.is_ascii_alphabetic() => {
                    if engine.ascii_mode() {
                        doc.insert_committed(c.to_string());
                    } else {
                        // Starting fresh: make sure Rime has no stale input
                        // buffer left from a backspaced-away composition.
                        if doc.composing.is_none() {
                            engine.reset();
                        }
                        doc.open_or_extend_composition(c);
                        engine.feed(c);
                        sync_after_engine(&mut doc, &mut engine);
                    }
                }
                // Catch-all for printable chars not handled above:
                // - ASCII non-letter symbols (`~ ^ & * ( ) - _ = + [ ] { } \ | / < >`
                //   etc.) commit raw in any mode.
                // - Non-ASCII chars (`ã é ü €` etc., usually composed by the OS/
                //   terminal before reaching us) commit raw only in Latin mode;
                //   in Chinese mode they'd just confuse Rime, so swallow them.
                (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT)
                    if !c.is_control() && (c.is_ascii() || engine.ascii_mode()) =>
                {
                    flush_composition(&mut doc, &mut engine);
                    doc.insert_committed(c.to_string());
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn paste_clipboard(doc: &mut Doc, clipboard: &mut arboard::Clipboard) {
    if let Ok(text) = clipboard.get_text() {
        // No dict validation: punctuation gets normalized to full-width to
        // match the in-app convention; everything else is committed verbatim.
        let mut buf = String::new();
        let mut buf_kind: Option<CharKind> = None;
        for ch in text.chars() {
            if ch == '\n' {
                flush_run(doc, &mut buf, &mut buf_kind);
                doc.break_line();
                continue;
            }
            let mapped = if is_ascii_punct(ch) {
                full_width_punct(ch)
            } else {
                ch
            };
            let kind = classify(mapped);
            if buf_kind != Some(kind) {
                flush_run(doc, &mut buf, &mut buf_kind);
                buf_kind = Some(kind);
            }
            buf.push(mapped);
        }
        flush_run(doc, &mut buf, &mut buf_kind);
    }
}

fn flush_run(doc: &mut Doc, buf: &mut String, kind: &mut Option<CharKind>) {
    if !buf.is_empty() {
        doc.insert_committed(std::mem::take(buf));
    }
    *kind = None;
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum CharKind {
    Cjk,
    Latin,
    Punct,
    Other,
}

fn classify(c: char) -> CharKind {
    if is_ascii_punct(c) || is_full_width_punct(c) {
        CharKind::Punct
    } else if c.is_ascii_alphanumeric() {
        CharKind::Latin
    } else if matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0x20000..=0x2A6DF) {
        CharKind::Cjk
    } else {
        CharKind::Other
    }
}

fn is_ascii_punct(c: char) -> bool {
    matches!(c, ',' | '.' | '?' | '!' | ':' | ';' | '\'' | '"')
}

fn is_full_width_punct(c: char) -> bool {
    matches!(
        c,
        '\u{3000}' | '，' | '。' | '？' | '！' | '：' | '；' | '＇' | '＂'
    )
}

fn full_width_punct(c: char) -> char {
    match c {
        ',' => '，',
        '.' => '。',
        '?' => '？',
        '!' => '！',
        ':' => '：',
        ';' => '；',
        '\'' => '＇',
        '"' => '＂',
        ' ' => '\u{3000}',
        _ => c,
    }
}

fn move_cursor_up(doc: &mut Doc, info: &ViewInfo) {
    if info.cursor_row == 0 {
        doc.cursor.1 = 0;
        return;
    }
    place_cursor_at_row(doc, info, info.cursor_row - 1, info.cursor_col);
}

fn move_cursor_down(doc: &mut Doc, info: &ViewInfo) {
    if info.cursor_row + 1 >= info.total_rows {
        let p = doc.cursor.0;
        doc.cursor.1 = doc.paragraphs[p].len();
        return;
    }
    place_cursor_at_row(doc, info, info.cursor_row + 1, info.cursor_col);
}

fn place_cursor_at_row(doc: &mut Doc, info: &ViewInfo, target_row: usize, target_col: usize) {
    let mut row_acc = 0usize;
    for (p_idx, pl) in info.layouts.iter().enumerate() {
        let n_rows = pl.rows.len().max(1);
        if row_acc + n_rows > target_row {
            let row_in_para = target_row - row_acc;
            let new_seg = pl
                .rows
                .get(row_in_para)
                .and_then(|row| {
                    row.pieces
                        .iter()
                        .min_by_key(|piece| piece.col_start.abs_diff(target_col))
                        .map(|p| p.seg_idx)
                })
                .unwrap_or(0);
            doc.cursor = (p_idx, new_seg);
            return;
        }
        row_acc += n_rows;
    }
    if let Some(last) = doc.paragraphs.len().checked_sub(1) {
        doc.cursor = (last, doc.paragraphs[last].len());
    }
}

fn main() -> Result<()> {
    if let Some(arg) = env::args().nth(1) {
        return match arg.as_str() {
            "--licenses" => {
                print_licenses();
                Ok(())
            }
            "--forget" => forget_user_dict(),
            "--help" | "-h" => {
                print_help();
                Ok(())
            }
            "--version" | "-V" => {
                println!("zipin {}", env!("CARGO_PKG_VERSION"));
                Ok(())
            }
            other => Err(anyhow!("unknown argument: {other}\n\n{}", help_text())),
        };
    }

    let engine = Engine::new()?;
    let mut terminal = ratatui::init();
    let res = run(&mut terminal, engine);
    ratatui::restore();
    // Don't leave the next shell prompt stuck with our chosen cursor shape.
    let _ = execute!(io::stdout(), SetCursorStyle::DefaultUserShape);
    res
}

fn print_help() {
    print!("{}", help_text());
}

fn help_text() -> &'static str {
    "\
zipin — terminal Chinese input method.

USAGE:
    zipin [OPTIONS]

OPTIONS:
    -h, --help        Print this help and exit.
    -V, --version     Print version and exit.
        --licenses    Print license + third-party attribution and exit.
        --forget      Wipe Rime user-dict files (learned phrases). Schemas
                      are preserved; deployed .bin files re-build on next
                      launch.
"
}

fn print_licenses() {
    println!("zipin\n=====\n\n{}", licenses::ZIPIN);
    println!("\n\n{}", licenses::THIRD_PARTY);
    println!("\n\n{}", licenses::NOTICE);
}

fn forget_user_dict() -> Result<()> {
    let dirs = engine::data_dirs()?;
    if !dirs.user.exists() {
        eprintln!(
            "zipin: no user data at {} — nothing to forget.",
            dirs.user.display()
        );
        return Ok(());
    }
    let mut wiped = 0usize;
    for entry in fs::read_dir(&dirs.user)? {
        let entry = entry?;
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // Per Rime conventions: per-schema user dbs live in `<id>.userdb/`,
        // sometimes with a `.userdb.txt` plain-text snapshot alongside.
        if path.is_dir() && name.ends_with(".userdb") {
            fs::remove_dir_all(&path)?;
            wiped += 1;
        } else if path.is_file() && (name.ends_with(".userdb.txt") || name == "user.yaml") {
            fs::remove_file(&path)?;
            wiped += 1;
        }
    }
    eprintln!(
        "zipin: forgot {wiped} user-dict entr{} under {}.",
        if wiped == 1 { "y" } else { "ies" },
        dirs.user.display()
    );
    Ok(())
}

/// Set the terminal cursor shape per language mode. Block for Chinese
/// (matches the visual weight of a hanzi cell), bar for English (thin
/// caret). Single global call; the terminal renders the chosen shape
/// wherever the cursor lands, so no per-frame work needed.
fn apply_cursor_style(ascii: bool) -> Result<()> {
    let mut out = io::stdout();
    if ascii {
        execute!(out, SetCursorStyle::SteadyBar)?;
    } else {
        execute!(out, SetCursorStyle::SteadyBlock)?;
    }
    out.flush()?;
    Ok(())
}

/// If a composition is active, commit the highlighted candidate into the
/// document. No-op otherwise. Used at the boundary of every key event that
/// "interrupts" a composition (Space, Enter, punctuation, Shift+letter).
fn flush_composition(doc: &mut Doc, engine: &mut Engine) {
    if engine.is_composing() {
        if let Some(text) = engine.commit_highlighted() {
            doc.commit_composition(text);
        }
    }
}

/// After feeding Rime a key, reconcile any auto-commit it produced and any
/// state mismatch between the doc's composing slot and Rime's view. Both
/// directions matter: if doc dropped its composing slot first (e.g. user
/// backspaced the whole composition away) and Rime still considers itself
/// composing, the next keystroke would extend the stale Rime input and
/// produce candidates for the wrong word.
fn sync_after_engine(doc: &mut Doc, engine: &mut Engine) {
    if let Some(text) = engine.take_commit() {
        doc.commit_composition(text);
    }
    if !engine.is_composing() && doc.composing.is_some() {
        doc.cancel_composition();
    } else if engine.is_composing() && doc.composing.is_none() {
        engine.reset();
    }
}
