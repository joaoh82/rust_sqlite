use std::fmt;
use std::path::{Path, PathBuf};

use rustyline::Editor;
use rustyline::history::DefaultHistory;

use crate::error::{Result, SQLRiteError};
use crate::repl::REPLHelper;
use crate::sql::db::database::Database;
use crate::sql::pager::{open_database, save_database};

#[derive(Debug, PartialEq)]
pub enum MetaCommand {
    Exit,
    Help,
    /// `.open FILENAME` — create or load a persistent database.
    Open(PathBuf),
    /// `.save FILENAME` — write the current database to disk.
    Save(PathBuf),
    /// `.tables` — list the tables in the current database.
    Tables,
    /// Parsed line that didn't match any known meta-command.
    Unknown,
}

/// Trait responsible for translating type into a formated text.
impl fmt::Display for MetaCommand {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            MetaCommand::Exit => f.write_str(".exit"),
            MetaCommand::Help => f.write_str(".help"),
            MetaCommand::Open(_) => f.write_str(".open"),
            MetaCommand::Save(_) => f.write_str(".save"),
            MetaCommand::Tables => f.write_str(".tables"),
            MetaCommand::Unknown => f.write_str("Unknown command"),
        }
    }
}

impl MetaCommand {
    pub fn new(command: String) -> MetaCommand {
        let args: Vec<&str> = command.split_whitespace().collect();
        let Some(cmd) = args.first() else {
            return MetaCommand::Unknown;
        };
        match *cmd {
            ".exit" => MetaCommand::Exit,
            ".help" => MetaCommand::Help,
            ".open" => match args.get(1) {
                Some(path) => MetaCommand::Open(PathBuf::from(path)),
                None => MetaCommand::Unknown,
            },
            ".save" => match args.get(1) {
                Some(path) => MetaCommand::Save(PathBuf::from(path)),
                None => MetaCommand::Unknown,
            },
            ".tables" => MetaCommand::Tables,
            _ => MetaCommand::Unknown,
        }
    }
}

/// Executes a parsed meta-command. May mutate `db` — `.open` replaces it
/// with the loaded file's database; `.save` just reads it.
pub fn handle_meta_command(
    command: MetaCommand,
    repl: &mut Editor<REPLHelper, DefaultHistory>,
    db: &mut Database,
) -> Result<String> {
    match command {
        MetaCommand::Exit => {
            repl.append_history("history").unwrap();
            std::process::exit(0)
        }
        MetaCommand::Help => Ok(format!(
            "{}{}{}{}{}{}{}",
            "Special commands:\n",
            ".help            - Display this message\n",
            ".open <FILENAME> - Open a SQLRite database file (creates it if missing)\n",
            ".save <FILENAME> - Write the current in-memory database to FILENAME\n",
            ".tables          - List tables in the current database\n",
            ".exit            - Quit this application\n",
            "\nOther meta commands (.read, .ast) are not implemented yet."
        )),
        MetaCommand::Open(path) => handle_open(&path, db),
        MetaCommand::Save(path) => handle_save(&path, db),
        MetaCommand::Tables => handle_tables(db),
        MetaCommand::Unknown => Err(SQLRiteError::UnknownCommand(
            "Unknown command or invalid arguments. Enter '.help'".to_string(),
        )),
    }
}

fn handle_open(path: &Path, db: &mut Database) -> Result<String> {
    let db_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("db")
        .to_string();
    if path.exists() {
        let loaded = open_database(path, db_name)?;
        let table_count = loaded.tables.len();
        *db = loaded;
        Ok(format!(
            "Opened '{}' ({table_count} table{s} loaded).",
            path.display(),
            s = if table_count == 1 { "" } else { "s" }
        ))
    } else {
        // Same behavior as SQLite: `.open` on a missing file creates a fresh
        // DB that will be materialized on the first `.save`.
        *db = Database::new(db_name);
        Ok(format!(
            "Opened '{}' (new database — use .save to persist).",
            path.display()
        ))
    }
}

fn handle_save(path: &Path, db: &Database) -> Result<String> {
    save_database(db, path)?;
    Ok(format!("Saved database to '{}'.", path.display()))
}

fn handle_tables(db: &Database) -> Result<String> {
    if db.tables.is_empty() {
        return Ok("(no tables)".to_string());
    }
    // Sort for deterministic output — HashMap iteration order is arbitrary.
    let mut names: Vec<&String> = db.tables.keys().collect();
    names.sort();
    Ok(names
        .into_iter()
        .map(|s| s.as_str())
        .collect::<Vec<&str>>()
        .join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repl::{REPLHelper, get_config};
    use crate::sql::process_command;

    fn new_editor() -> Editor<REPLHelper, DefaultHistory> {
        let config = get_config();
        let helper = REPLHelper::default();
        let mut repl: Editor<REPLHelper, DefaultHistory> =
            Editor::with_config(config).expect("failed to build rustyline editor");
        repl.set_helper(Some(helper));
        repl
    }

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!("sqlrite-meta-{pid}-{nanos}-{name}.sqlrite"));
        p
    }

    #[test]
    fn help_works() {
        let mut repl = new_editor();
        let mut db = Database::new("x".to_string());
        let result = handle_meta_command(MetaCommand::Help, &mut repl, &mut db);
        assert!(result.is_ok());
    }

    #[test]
    fn parse_open_requires_argument() {
        assert_eq!(MetaCommand::new(".open".to_string()), MetaCommand::Unknown);
        assert_eq!(
            MetaCommand::new(".open my.sqlrite".to_string()),
            MetaCommand::Open(PathBuf::from("my.sqlrite"))
        );
    }

    #[test]
    fn parse_save_requires_argument() {
        assert_eq!(MetaCommand::new(".save".to_string()), MetaCommand::Unknown);
        assert_eq!(
            MetaCommand::new(".save my.sqlrite".to_string()),
            MetaCommand::Save(PathBuf::from("my.sqlrite"))
        );
    }

    #[test]
    fn tables_meta_command() {
        let mut repl = new_editor();
        let mut db = Database::new("x".to_string());
        // Empty case.
        let msg = handle_meta_command(MetaCommand::Tables, &mut repl, &mut db).unwrap();
        assert_eq!(msg, "(no tables)");

        // Populated case — two tables, output should be sorted.
        process_command("CREATE TABLE zebras (id INTEGER PRIMARY KEY);", &mut db).unwrap();
        process_command("CREATE TABLE apples (id INTEGER PRIMARY KEY);", &mut db).unwrap();
        let msg = handle_meta_command(MetaCommand::Tables, &mut repl, &mut db).unwrap();
        assert_eq!(msg, "apples\nzebras");
    }

    #[test]
    fn save_then_open_round_trips_through_meta_commands() {
        use crate::sql::db::table::Value;

        let path = tmp_path("meta_roundtrip");
        let mut repl = new_editor();
        let mut db = Database::new("x".to_string());

        process_command(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
            &mut db,
        )
        .unwrap();
        process_command("INSERT INTO users (name) VALUES ('alice');", &mut db).unwrap();

        handle_meta_command(MetaCommand::Save(path.clone()), &mut repl, &mut db)
            .expect("save");

        // Replace db with a fresh one, then .open the file.
        db = Database::new("fresh".to_string());
        let msg = handle_meta_command(MetaCommand::Open(path.clone()), &mut repl, &mut db)
            .expect("open");
        assert!(msg.contains("1 table loaded"));

        let users = db.get_table("users".to_string()).unwrap();
        let rowids = users.rowids();
        assert_eq!(rowids.len(), 1);
        assert_eq!(
            users.get_value("name", rowids[0]),
            Some(Value::Text("alice".to_string()))
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_missing_file_creates_empty_db() {
        let path = tmp_path("missing");
        let mut repl = new_editor();
        let mut db = Database::new("x".to_string());

        let msg = handle_meta_command(MetaCommand::Open(path.clone()), &mut repl, &mut db)
            .expect("open");
        assert!(msg.contains("new database"));
        assert_eq!(db.tables.len(), 0);
        // File should not have been created yet (save is explicit).
        assert!(!path.exists());
    }
}
