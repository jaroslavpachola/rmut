//! Real Neovim, embedded: `nvim --embed` as a child on msgpack-RPC,
//! its `ext_linegrid` screen rendered by the window and every key
//! forwarded in nvim notation. Not an imitation - the user's own
//! nvim, config, plugins and all, with the window as its terminal.

use std::collections::HashMap;
use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;

use rmpv::Value;
use rmut_front::{KeyCode, KeyEvent, KeyModifiers};

/// One grid cell: the character and its highlight id.
#[derive(Clone)]
pub struct Cell {
    pub text: String,
    pub hl: u64,
}

impl Default for Cell {
    fn default() -> Cell {
        Cell {
            text: " ".into(),
            hl: 0,
        }
    }
}

/// A highlight attribute, from `hl_attr_define`.
#[derive(Clone, Copy, Default)]
pub struct Attr {
    pub fg: Option<u32>,
    pub bg: Option<u32>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
}

/// The single linegrid (no multigrid: nvim composites everything
/// into grid 1 when only ext_linegrid is on).
pub struct Grid {
    pub cells: Vec<Vec<Cell>>,
    pub cols: usize,
    pub rows: usize,
    pub cursor: (usize, usize),
}

impl Grid {
    fn new(cols: usize, rows: usize) -> Grid {
        Grid {
            cells: vec![vec![Cell::default(); cols]; rows],
            cols,
            rows,
            cursor: (0, 0),
        }
    }
}

/// The embedded instance: the child, its stdin for our requests, and
/// the reader thread's channel of parsed messages.
pub struct Embedded {
    child: Child,
    stdin: ChildStdin,
    rx: mpsc::Receiver<Value>,
    msgid: u64,
    pub grid: Grid,
    pub attrs: HashMap<u64, Attr>,
    pub default_fg: u32,
    pub default_bg: u32,
    /// The reader saw EOF: nvim is gone (`:wq`, `:q!`, a crash).
    pub finished: bool,
}

impl Embedded {
    /// Spawn `nvim --embed FILE` and attach the UI at `cols`x`rows`.
    /// `wake` is called from the reader thread whenever nvim has
    /// something to show, so the frame loop repaints promptly.
    pub fn start(
        file: &std::path::Path,
        cols: usize,
        rows: usize,
        wake: impl Fn() + Send + 'static,
    ) -> std::io::Result<Embedded> {
        Self::start_with(&[], file, cols, rows, wake)
    }

    /// `start` with arguments of its own ahead of `--embed`: a test
    /// passes `--clean`, so it drives nvim itself and not whatever
    /// the user's config does on startup.
    pub fn start_with(
        args: &[&str],
        file: &std::path::Path,
        cols: usize,
        rows: usize,
        wake: impl Fn() + Send + 'static,
    ) -> std::io::Result<Embedded> {
        let mut child = Command::new("nvim")
            .args(args)
            .arg("--embed")
            .arg(file)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = std::io::BufReader::new(stdout);
            while let Ok(value) = rmpv::decode::read_value(&mut reader) {
                let closed = tx.send(value).is_err();
                wake();
                if closed {
                    return;
                }
            }
            let _ = tx.send(Value::Nil); // EOF marker
            wake();
        });
        let mut nvim = Embedded {
            child,
            stdin,
            rx,
            msgid: 0,
            grid: Grid::new(cols, rows),
            attrs: HashMap::new(),
            default_fg: 0xd8d8d8,
            default_bg: 0x101010,
            finished: false,
        };
        nvim.request(
            "nvim_ui_attach",
            vec![
                Value::from(cols as u64),
                Value::from(rows as u64),
                Value::Map(vec![
                    (Value::from("rgb"), Value::from(true)),
                    (Value::from("ext_linegrid"), Value::from(true)),
                ]),
            ],
        );
        Ok(nvim)
    }

    /// A request whose response nobody waits for: the reader drains
    /// it, and an error lands in the log rather than a deadlock.
    fn request(&mut self, method: &str, params: Vec<Value>) {
        self.msgid += 1;
        let msg = Value::Array(vec![
            Value::from(0),
            Value::from(self.msgid),
            Value::from(method),
            Value::Array(params),
        ]);
        let _ = rmpv::encode::write_value(&mut self.stdin, &msg);
        let _ = self.stdin.flush();
    }

    pub fn input(&mut self, keys: &str) {
        self.request("nvim_input", vec![Value::from(keys)]);
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        if (cols, rows) != (self.grid.cols, self.grid.rows) && cols > 0 && rows > 0 {
            self.request(
                "nvim_ui_try_resize",
                vec![Value::from(cols as u64), Value::from(rows as u64)],
            );
        }
    }

    /// Drain the reader's channel and apply every redraw batch.
    /// Returns true when anything changed on screen.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        while let Ok(value) = self.rx.try_recv() {
            if value.is_nil() {
                self.finished = true;
                changed = true;
                continue;
            }
            let Value::Array(items) = value else { continue };
            // [2, "redraw", [events...]] - anything else is a
            // response we do not wait for (log errors) or a request
            // nvim should not send an embedder.
            match items.as_slice() {
                [kind, method, params] if kind.as_u64() == Some(2) => {
                    if method.as_str() == Some("redraw")
                        && let Value::Array(events) = params
                    {
                        for event in events {
                            changed |= self.apply(event);
                        }
                    }
                }
                [kind, _id, error, _result] if kind.as_u64() == Some(1) && !error.is_nil() => {
                    eprintln!("rmut-egui: nvim: {error}");
                }
                _ => {}
            }
        }
        if self.child.try_wait().is_ok_and(|s| s.is_some()) {
            self.finished = true;
            changed = true;
        }
        changed
    }

    /// One redraw event: ["name", args, args, ...].
    fn apply(&mut self, event: &Value) -> bool {
        let Value::Array(parts) = event else {
            return false;
        };
        let Some(name) = parts.first().and_then(Value::as_str) else {
            return false;
        };
        let mut changed = false;
        for args in &parts[1..] {
            let Value::Array(args) = args else { continue };
            changed |= self.apply_one(name, args);
        }
        changed
    }

    fn apply_one(&mut self, name: &str, args: &[Value]) -> bool {
        let n = |i: usize| args.get(i).and_then(Value::as_u64).unwrap_or(0) as usize;
        match name {
            "grid_resize" => {
                let (cols, rows) = (n(1), n(2));
                let mut grid = Grid::new(cols, rows);
                for (row, old) in grid.cells.iter_mut().zip(&self.grid.cells) {
                    for (cell, old) in row.iter_mut().zip(old) {
                        *cell = old.clone();
                    }
                }
                grid.cursor = self.grid.cursor;
                self.grid = grid;
                true
            }
            "grid_clear" => {
                self.grid = Grid::new(self.grid.cols, self.grid.rows);
                true
            }
            "grid_cursor_goto" => {
                self.grid.cursor = (n(2), n(3)).min((self.grid.rows, self.grid.cols));
                self.grid.cursor = (n(2), n(3));
                true
            }
            "default_colors_set" => {
                if let Some(fg) = args.get(1).and_then(Value::as_u64) {
                    self.default_fg = fg as u32;
                }
                if let Some(bg) = args.get(2).and_then(Value::as_u64) {
                    self.default_bg = bg as u32;
                }
                true
            }
            "hl_attr_define" => {
                let id = n(1);
                let mut attr = Attr::default();
                if let Some(Value::Map(map)) = args.get(2) {
                    for (key, value) in map {
                        match key.as_str() {
                            Some("foreground") => attr.fg = value.as_u64().map(|v| v as u32),
                            Some("background") => attr.bg = value.as_u64().map(|v| v as u32),
                            Some("bold") => attr.bold = value.as_bool().unwrap_or(false),
                            Some("italic") => attr.italic = value.as_bool().unwrap_or(false),
                            Some("underline") => attr.underline = value.as_bool().unwrap_or(false),
                            Some("reverse") => attr.reverse = value.as_bool().unwrap_or(false),
                            _ => {}
                        }
                    }
                }
                self.attrs.insert(id as u64, attr);
                false
            }
            "grid_line" => {
                self.grid_line(args);
                true
            }
            "grid_scroll" => {
                self.grid_scroll(args);
                true
            }
            _ => false,
        }
    }

    /// [grid, row, col_start, cells, wrap]: cells are [text, hl?,
    /// repeat?], the highlight carrying over when omitted.
    fn grid_line(&mut self, args: &[Value]) {
        let row = args.get(1).and_then(Value::as_u64).unwrap_or(0) as usize;
        let mut col = args.get(2).and_then(Value::as_u64).unwrap_or(0) as usize;
        let Some(Value::Array(cells)) = args.get(3) else {
            return;
        };
        let Some(line) = self.grid.cells.get_mut(row) else {
            return;
        };
        let mut hl = 0u64;
        for cell in cells {
            let Value::Array(parts) = cell else { continue };
            let text = parts.first().and_then(Value::as_str).unwrap_or(" ");
            if let Some(id) = parts.get(1).and_then(Value::as_u64) {
                hl = id;
            }
            let repeat = parts.get(2).and_then(Value::as_u64).unwrap_or(1) as usize;
            for _ in 0..repeat {
                if let Some(slot) = line.get_mut(col) {
                    *slot = Cell {
                        text: text.to_string(),
                        hl,
                    };
                }
                col += 1;
            }
        }
    }

    /// [grid, top, bot, left, right, rows, cols]: the region moves by
    /// `rows` (positive: content scrolls up).
    fn grid_scroll(&mut self, args: &[Value]) {
        let v = |i: usize| args.get(i).and_then(Value::as_i64).unwrap_or(0);
        let (top, bot, left, right) = (
            v(1).max(0) as usize,
            v(2).max(0) as usize,
            v(3).max(0) as usize,
            v(4).max(0) as usize,
        );
        let rows = v(5);
        let bot = bot.min(self.grid.rows);
        let right = right.min(self.grid.cols);
        if rows > 0 {
            for dst in top..bot.saturating_sub(rows as usize) {
                let src = dst + rows as usize;
                let (a, b) = self.grid.cells.split_at_mut(src);
                a[dst][left..right].clone_from_slice(&b[0][left..right]);
            }
        } else if rows < 0 {
            let up = (-rows) as usize;
            for dst in (top + up..bot).rev() {
                let src = dst - up;
                let (a, b) = self.grid.cells.split_at_mut(dst);
                b[0][left..right].clone_from_slice(&a[src][left..right]);
            }
        }
    }
}

impl Drop for Embedded {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A front-end key in nvim's input notation.
pub fn notation(key: &KeyEvent) -> Option<String> {
    let named = match key.code {
        KeyCode::Enter => Some("CR"),
        KeyCode::Esc => Some("Esc"),
        KeyCode::Tab => Some("Tab"),
        KeyCode::Backspace => Some("BS"),
        KeyCode::Delete => Some("Del"),
        KeyCode::Up => Some("Up"),
        KeyCode::Down => Some("Down"),
        KeyCode::Left => Some("Left"),
        KeyCode::Right => Some("Right"),
        KeyCode::PageUp => Some("PageUp"),
        KeyCode::PageDown => Some("PageDown"),
        KeyCode::Home => Some("Home"),
        KeyCode::End => Some("End"),
        KeyCode::Char(_) | KeyCode::Other => None,
    };
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    if let Some(name) = named {
        let mut mods = String::new();
        if ctrl {
            mods += "C-";
        }
        if alt {
            mods += "M-";
        }
        if shift {
            mods += "S-";
        }
        return Some(format!("<{mods}{name}>"));
    }
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    if ctrl || alt {
        let mut mods = String::new();
        if ctrl {
            mods += "C-";
        }
        if alt {
            mods += "M-";
        }
        return Some(format!("<{mods}{c}>"));
    }
    Some(match c {
        '<' => "<lt>".to_string(),
        c => c.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn keys_speak_nvim_notation() {
        assert_eq!(
            notation(&ev(KeyCode::Char('a'), KeyModifiers::NONE)).unwrap(),
            "a"
        );
        assert_eq!(
            notation(&ev(KeyCode::Char('<'), KeyModifiers::NONE)).unwrap(),
            "<lt>"
        );
        assert_eq!(
            notation(&ev(KeyCode::Char('w'), KeyModifiers::CONTROL)).unwrap(),
            "<C-w>"
        );
        assert_eq!(
            notation(&ev(KeyCode::Esc, KeyModifiers::NONE)).unwrap(),
            "<Esc>"
        );
        assert_eq!(
            notation(&ev(KeyCode::Enter, KeyModifiers::NONE)).unwrap(),
            "<CR>"
        );
        assert_eq!(
            notation(&ev(KeyCode::Tab, KeyModifiers::SHIFT)).unwrap(),
            "<S-Tab>"
        );
        assert_eq!(
            notation(&ev(
                KeyCode::Char('x'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            ))
            .unwrap(),
            "<C-M-x>"
        );
    }

    #[test]
    fn grid_lines_and_scroll_apply() {
        let mut nvim = fake();
        // "hi" at row 0, then a repeat run of dashes with a carried
        // highlight.
        nvim.grid_line(&[
            Value::from(1),
            Value::from(0),
            Value::from(0),
            Value::Array(vec![
                Value::Array(vec![Value::from("h"), Value::from(7)]),
                Value::Array(vec![Value::from("i")]),
                Value::Array(vec![Value::from("-"), Value::from(2), Value::from(3)]),
            ]),
        ]);
        let row: String = nvim.grid.cells[0].iter().map(|c| c.text.as_str()).collect();
        assert_eq!(&row[..5], "hi---");
        assert_eq!(nvim.grid.cells[0][1].hl, 7, "the highlight carries over");
        assert_eq!(nvim.grid.cells[0][2].hl, 2);
        // Scroll the top two rows up by one: row 1 moves into row 0.
        nvim.grid_line(&[
            Value::from(1),
            Value::from(1),
            Value::from(0),
            Value::Array(vec![Value::Array(vec![Value::from("x")])]),
        ]);
        nvim.grid_scroll(&[
            Value::from(1),
            Value::from(0),
            Value::from(2),
            Value::from(0),
            Value::from(10),
            Value::from(1),
            Value::from(0),
        ]);
        assert_eq!(nvim.grid.cells[0][0].text, "x");
    }

    /// An Embedded with a dead child, for the pure grid logic.
    fn fake() -> Embedded {
        let mut child = Command::new("true")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let (_tx, rx) = mpsc::channel();
        Embedded {
            child,
            stdin,
            rx,
            msgid: 0,
            grid: Grid::new(10, 2),
            attrs: HashMap::new(),
            default_fg: 0,
            default_bg: 0,
            finished: false,
        }
    }
}
