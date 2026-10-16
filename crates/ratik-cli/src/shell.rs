//! `ratik shell`: an interactive RQL prompt.

use std::time::Instant;

use ratik::{Ratik, Rows};
use rustyline::error::ReadlineError;

use crate::{brand, paths, table};

const HELP: &str = "\
  Statements end with ';' and may span lines. Meta commands:

    \\d             list node and edge types
    \\d <Type>      describe a type
    \\schema        print the schema as DDL
    \\queries       list stored queries
    \\stats         database statistics
    \\json          toggle JSON output
    \\timing        toggle timing
    \\clear         clear the screen
    \\q             quit

  Try:
    CREATE NODE Person (name STRING NOT NULL UNIQUE, age INT);
    INSERT INTO Person (name, age) VALUES ('Ada', 36), ('Alan', 41);
    SELECT name FROM Person WHERE age > 40;";

pub struct Shell {
    db: Ratik,
    json: bool,
    timing: bool,
}

impl Shell {
    pub fn new(db: Ratik) -> Self {
        Self {
            db,
            json: false,
            timing: true,
        }
    }

    /// Runs one statement (or script) and prints the result.
    pub fn run(&self, rql: &str) -> bool {
        let started = Instant::now();
        match self.db.execute(rql) {
            Ok(rows) => {
                self.print(&rows, started.elapsed().as_secs_f64() * 1000.0);
                true
            }
            Err(e) => {
                eprintln!("{}", brand::err(&e.to_string()));
                false
            }
        }
    }

    fn print(&self, rows: &Rows, ms: f64) {
        print(rows, ms, self.json, self.timing);
    }

    fn meta(&mut self, cmd: &str) -> bool {
        let mut parts = cmd.split_whitespace();
        match (parts.next().unwrap_or_default(), parts.next()) {
            ("\\q" | "\\quit" | "\\exit", _) => return false,
            ("\\?" | "\\h" | "\\help", _) => println!("{HELP}"),
            ("\\d", None) => {
                self.run("SHOW NODES");
                self.run("SHOW EDGES");
            }
            ("\\d", Some(t)) => {
                self.run(&format!("DESCRIBE `{t}`"));
            }
            ("\\schema", _) => {
                self.run("SHOW SCHEMA");
            }
            ("\\queries", _) => {
                self.run("SHOW QUERIES");
            }
            ("\\stats", _) => {
                self.run("SHOW STATS");
            }
            ("\\indexes", _) => {
                self.run("SHOW INDEXES");
            }
            ("\\json", _) => {
                self.json = !self.json;
                println!(
                    "{}",
                    brand::ok(&format!(
                        "JSON output {}",
                        if self.json { "on" } else { "off" }
                    ))
                );
            }
            ("\\timing", _) => {
                self.timing = !self.timing;
                println!(
                    "{}",
                    brand::ok(&format!(
                        "timing {}",
                        if self.timing { "on" } else { "off" }
                    ))
                );
            }
            ("\\clear", _) => print!("\x1b[2J\x1b[H"),
            (other, _) => eprintln!(
                "{}",
                brand::err(&format!("unknown command {other} — try \\?"))
            ),
        }
        true
    }

    pub fn repl(mut self, target: &str) -> anyhow::Result<()> {
        println!("{}", brand::banner());
        println!(
            "{}",
            brand::kv("connected", &brand::rgb(target, brand::CYAN))
        );
        println!("{}\n", brand::kv("help", "\\? for help, \\q to quit"));
        let mut rl = rustyline::DefaultEditor::new()?;
        let history = paths::history_file();
        let _ = rl.load_history(&history);
        let mut buf = String::new();
        loop {
            let prompt = if buf.is_empty() {
                format!("{} ", brand::gradient("ratik ›"))
            } else {
                format!("{} ", brand::dim("   ... "))
            };
            match rl.readline(&prompt) {
                Ok(line) => {
                    let trimmed = line.trim();
                    if buf.is_empty() && trimmed.starts_with('\\') {
                        let _ = rl.add_history_entry(trimmed);
                        if !self.meta(trimmed) {
                            break;
                        }
                        continue;
                    }
                    if buf.is_empty()
                        && matches!(trimmed.to_ascii_lowercase().as_str(), "exit" | "quit")
                    {
                        break;
                    }
                    if trimmed.is_empty() && buf.is_empty() {
                        continue;
                    }
                    buf.push_str(&line);
                    buf.push('\n');
                    if trimmed.ends_with(';') {
                        let stmt = std::mem::take(&mut buf);
                        let _ = rl.add_history_entry(stmt.trim());
                        self.run(&stmt);
                    }
                }
                Err(ReadlineError::Interrupted) => {
                    if buf.is_empty() {
                        println!("{}", brand::dim("(use \\q to quit)"));
                    }
                    buf.clear();
                }
                Err(ReadlineError::Eof) => break,
                Err(e) => return Err(e.into()),
            }
        }
        let _ = rl.save_history(&history);
        println!("{}", brand::dim("bye 👋"));
        Ok(())
    }
}

/// Prints a result the friendly way: status lines, plans and DDL as text,
/// everything else as a table (or JSON).
pub fn print(rows: &Rows, ms: f64, json: bool, timing: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&rows.to_objects()).unwrap_or_default()
        );
        return;
    }
    // Status-only results (DDL) print as one friendly line.
    if rows.columns == ["status"] {
        for r in rows {
            println!(
                "{}",
                brand::ok(&r.get::<String>("status").unwrap_or_default())
            );
        }
        return;
    }
    if rows.columns == ["plan"] {
        for r in rows {
            println!(
                "  {}",
                brand::rgb(&r.get::<String>("plan").unwrap_or_default(), brand::CYAN)
            );
        }
        return;
    }
    if rows.columns == ["ddl"] {
        for r in rows {
            println!("{}\n", r.get::<String>("ddl").unwrap_or_default());
        }
        return;
    }
    let rendered = table::render(rows);
    if !rendered.is_empty() {
        println!("{rendered}");
    }
    if timing || rows.columns.is_empty() {
        println!("{}", table::footer(rows, ms));
    }
}
