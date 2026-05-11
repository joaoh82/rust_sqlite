use crate::meta_command::*;
use sqlrite::Connection;
use sqlrite::sql::SQLCommand;
use sqlrite::sql::db::database::Database;

use std::borrow::Cow::{self, Borrowed, Owned};
use std::sync::MutexGuard;

use rustyline::error::ReadlineError;
use rustyline::highlight::{CmdKind, Highlighter, MatchingBracketHighlighter};
use rustyline::hint::{Hinter, HistoryHinter};
use rustyline::validate::Validator;
use rustyline::validate::{ValidationContext, ValidationResult};
use rustyline::{CompletionType, Config, Context, EditMode};
use rustyline_derive::{Completer, Helper};

/// We have two different types of commands MetaCommand and SQLCommand
#[derive(Debug, PartialEq)]
pub enum CommandType {
    MetaCommand(MetaCommand),
    SQLCommand(SQLCommand),
}

/// Returns the type of command inputed in the REPL
pub fn get_command_type(command: &str) -> CommandType {
    if command.starts_with('.') {
        CommandType::MetaCommand(MetaCommand::new(command.to_owned()))
    } else {
        CommandType::SQLCommand(SQLCommand::new(command.to_owned()))
    }
}

// REPL Helper Struct with all functionalities
#[derive(Helper, Completer)]
pub struct REPLHelper {
    // pub validator: MatchingBracketValidator,
    pub colored_prompt: String,
    pub hinter: HistoryHinter,
    pub highlighter: MatchingBracketHighlighter,
}

// Implementing the Default trait to give our struct a default value
impl Default for REPLHelper {
    fn default() -> Self {
        Self {
            highlighter: MatchingBracketHighlighter::new(),
            hinter: HistoryHinter::new(),
            colored_prompt: "".to_owned(),
        }
    }
}

// Implementing trait responsible for providing hints
impl Hinter for REPLHelper {
    type Hint = String;

    // Takes the currently edited line with the cursor position and returns the string that should be
    // displayed or None if no hint is available for the text the user currently typed
    fn hint(&self, line: &str, pos: usize, ctx: &Context<'_>) -> Option<String> {
        self.hinter.hint(line, pos, ctx)
    }
}

// Implementing trait responsible for determining whether the current input buffer is valid.
// Rustyline uses the method provided by this trait to decide whether hitting the enter key
// will end the current editing session and return the current line buffer to the caller of
// Editor::readline or variants.
impl Validator for REPLHelper {
    // Takes the currently edited input and returns a ValidationResult indicating whether it
    // is valid or not along with an option message to display about the result.
    fn validate(&self, ctx: &mut ValidationContext) -> Result<ValidationResult, ReadlineError> {
        use ValidationResult::{Incomplete, /*Invalid,*/ Valid};
        let input = ctx.input();
        let result = if input.starts_with('.') {
            Valid(None)
        } else if !input.ends_with(';') {
            Incomplete
        } else {
            Valid(None)
        };
        Ok(result)
    }
}

// Implementing syntax highlighter with ANSI color.
impl Highlighter for REPLHelper {
    // Takes the prompt and returns the highlighted version (with ANSI color).
    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        default: bool,
    ) -> Cow<'b, str> {
        if default {
            Borrowed(&self.colored_prompt)
        } else {
            Borrowed(prompt)
        }
    }

    // Takes the hint and returns the highlighted version (with ANSI color).
    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        Owned("\x1b[1m".to_owned() + hint + "\x1b[m")
    }

    // Takes the currently edited line with the cursor position and returns the highlighted version (with ANSI color).
    fn highlight<'l>(&self, line: &'l str, pos: usize) -> Cow<'l, str> {
        self.highlighter.highlight(line, pos)
    }

    // Tells if line needs to be highlighted when a specific char is typed or when cursor is moved under a specific char.
    // Used to optimize refresh when a character is inserted or the cursor is moved.
    fn highlight_char(&self, line: &str, pos: usize, kind: CmdKind) -> bool {
        self.highlighter.highlight_char(line, pos, kind)
    }
}

// Returns a Config::builder with basic Editor configuration
pub fn get_config() -> Config {
    Config::builder()
        .history_ignore_space(true)
        .completion_type(CompletionType::List)
        .edit_mode(EditMode::Emacs)
        .build()
}

/// Multi-handle REPL state (Phase 11.11a).
///
/// Holds every `Connection` the user has minted (`.spawn`) plus the
/// index of the currently-active one. `.spawn` is the headline
/// feature: it appends a sibling handle that shares the same
/// `Arc<Mutex<Database>>` so the user can drive interactive
/// `BEGIN CONCURRENT` demos — open a tx on `A`, write the same row
/// on `B`, watch `A`'s commit lose to `B`'s, and so on.
///
/// **Naming.** Handles are named `A`, `B`, `C`, …, `Z`, `AA`, `AB`,
/// … in order of creation. Names never get reused inside a session
/// even if a handle is later dropped (no `.drop` exists yet — but
/// future work could keep the names monotonic regardless).
pub struct ReplState {
    conns: Vec<Connection>,
    /// Per-handle display name, parallel to `conns`. Stable across
    /// `.use` switches.
    names: Vec<String>,
    /// Index into `conns` (and `names`) of the active handle.
    /// Mutated by `.use NAME`; every SQL statement and most meta-
    /// commands route through here.
    active: usize,
}

impl ReplState {
    /// Builds a fresh REPL state with one connection named `A`.
    pub fn new(conn: Connection) -> Self {
        Self {
            conns: vec![conn],
            names: vec!["A".to_string()],
            active: 0,
        }
    }

    /// The currently-active handle's name (`A`, `B`, …) — used in
    /// the prompt and in `.conns` output.
    pub fn active_name(&self) -> &str {
        &self.names[self.active]
    }

    /// All `(name, in_concurrent_tx)` tuples, in creation order.
    /// Used by `.conns`. `in_concurrent_tx` reflects whether the
    /// handle currently has an open `BEGIN CONCURRENT` — useful for
    /// demos so the user can see which siblings are mid-tx.
    pub fn handles_summary(&self) -> Vec<(String, bool)> {
        self.conns
            .iter()
            .zip(self.names.iter())
            .map(|(c, n)| (n.clone(), c.concurrent_tx_is_open()))
            .collect()
    }

    /// Locks the active handle's database and returns the guard.
    /// Used by meta-commands that need to mutate the underlying
    /// `Database` directly (`.open`, `.save`, `.tables`, `.ask`).
    pub fn lock_active(&self) -> MutexGuard<'_, Database> {
        self.conns[self.active].database()
    }

    /// Mutable handle to the active `Connection`. The REPL's SQL
    /// dispatch routes through this so `Connection::execute_with_render`
    /// catches `BEGIN CONCURRENT` / `COMMIT` / `ROLLBACK` and the
    /// per-connection MVCC state stays in sync.
    pub fn active_conn_mut(&mut self) -> &mut Connection {
        &mut self.conns[self.active]
    }

    /// Mints a new sibling handle off the active one and switches
    /// to it. Returns the new handle's name. Backs `.spawn`.
    pub fn spawn_sibling(&mut self) -> String {
        let sibling = self.conns[self.active].connect();
        let name = next_handle_name(self.conns.len());
        self.conns.push(sibling);
        self.names.push(name.clone());
        self.active = self.conns.len() - 1;
        name
    }

    /// Switches the active handle to the one whose display name
    /// matches `target` (case-insensitive). Returns `Ok(name)` if
    /// found; `Err(msg)` with a list of valid names otherwise.
    pub fn use_handle(&mut self, target: &str) -> Result<String, String> {
        let target_upper = target.to_ascii_uppercase();
        if let Some(idx) = self.names.iter().position(|n| *n == target_upper) {
            self.active = idx;
            Ok(self.names[idx].clone())
        } else {
            let valid = self.names.join(", ");
            Err(format!(
                "no handle named '{target}'; current handles: {valid}"
            ))
        }
    }

    /// Number of live sibling handles. Used by `.open` to decide
    /// whether replacing the underlying Database is safe.
    pub fn handle_count(&self) -> usize {
        self.conns.len()
    }

    /// Drops every sibling, keeping only handle `A`. Used by
    /// `.open` so the new database doesn't strand siblings pointing
    /// at the old one.
    pub fn collapse_to_active(&mut self) {
        if self.conns.len() == 1 {
            return;
        }
        // Keep the *active* handle (so `.open` from any handle
        // works), rename it to `A`, drop the rest.
        let kept = self.conns.swap_remove(self.active);
        self.conns.clear();
        self.names.clear();
        self.conns.push(kept);
        self.names.push("A".to_string());
        self.active = 0;
    }
}

/// Returns the display name for the i-th spawned handle:
/// `0 -> A`, `1 -> B`, …, `25 -> Z`, `26 -> AA`, `27 -> AB`, …
fn next_handle_name(index: usize) -> String {
    let mut n = index;
    let mut out = String::new();
    loop {
        let r = n % 26;
        out.insert(0, (b'A' + r as u8) as char);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_command_type_meta_command_test() {
        let input = String::from(".help");
        let expected = CommandType::MetaCommand(MetaCommand::Help);

        let result = get_command_type(&input);
        assert_eq!(result, expected);
    }

    #[test]
    fn get_command_type_sql_command_test() {
        let input = String::from("SELECT * from users;");
        let expected = CommandType::SQLCommand(SQLCommand::Unknown(input.clone()));

        let result = get_command_type(&input);
        assert_eq!(result, expected);
    }
}
