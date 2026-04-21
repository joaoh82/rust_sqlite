use crate::error::{Result, SQLRiteError};

use crate::repl::REPLHelper;
use rustyline::Editor;
use rustyline::history::DefaultHistory;
use std::fmt;

#[derive(Debug, PartialEq)]
pub enum MetaCommand {
    Exit,
    Help,
    Open(String),
    Unknown,
}

/// Trait responsible for translating type into a formated text.
impl fmt::Display for MetaCommand {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            MetaCommand::Exit => f.write_str(".exit"),
            MetaCommand::Help => f.write_str(".help"),
            MetaCommand::Open(_) => f.write_str(".open"),
            MetaCommand::Unknown => f.write_str("Unknown command"),
        }
    }
}

impl MetaCommand {
    pub fn new(command: String) -> MetaCommand {
        let args: Vec<&str> = command.split_whitespace().collect();
        let cmd = args[0].to_owned();
        match cmd.as_ref() {
            ".exit" => MetaCommand::Exit,
            ".help" => MetaCommand::Help,
            ".open" => MetaCommand::Open(command),
            _ => MetaCommand::Unknown,
        }
    }
}

pub fn handle_meta_command(
    command: MetaCommand,
    repl: &mut Editor<REPLHelper, DefaultHistory>,
) -> Result<String> {
    match command {
        MetaCommand::Exit => {
            repl.append_history("history").unwrap();
            std::process::exit(0)
        }
        MetaCommand::Help => Ok(format!(
            "{}{}{}{}{}{}{}{}",
            "Special commands:\n",
            ".help            - Display this message\n",
            ".open <FILENAME> - Close existing database and reopen FILENAME\n",
            ".save <FILENAME> - Write in-memory database into FILENAME\n",
            ".read <FILENAME> - Read input from FILENAME\n",
            ".tables          - List names of tables\n",
            ".ast <QUERY>     - Show the abstract syntax tree for QUERY.\n",
            ".exit            - Quits this application"
        )),
        MetaCommand::Open(args) => Ok(format!("To be implemented: {args}")),
        MetaCommand::Unknown => Err(SQLRiteError::UnknownCommand(
            "Unknown command or invalid arguments. Enter '.help'".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repl::{REPLHelper, get_config};

    fn new_editor() -> Editor<REPLHelper, DefaultHistory> {
        let config = get_config();
        let helper = REPLHelper::default();
        let mut repl: Editor<REPLHelper, DefaultHistory> =
            Editor::with_config(config).expect("failed to build rustyline editor");
        repl.set_helper(Some(helper));
        repl
    }

    #[test]
    fn get_meta_command_exit_test() {
        let mut repl = new_editor();

        let inputed_command = MetaCommand::Help;

        let result = handle_meta_command(inputed_command, &mut repl);
        assert!(result.is_ok());
    }

    #[test]
    fn get_meta_command_open_test() {
        let mut repl = new_editor();

        let inputed_command = MetaCommand::Open(".open database.db".to_string());

        let result = handle_meta_command(inputed_command, &mut repl);
        assert!(result.is_ok());
    }

    #[test]
    fn get_meta_command_unknown_command_test() {
        let mut repl = new_editor();

        let inputed_command = MetaCommand::Unknown;

        let result = handle_meta_command(inputed_command, &mut repl);
        assert!(result.is_err());
    }

    #[test]
    fn meta_command_display_trait_test() {
        let exit = MetaCommand::Exit;
        let help = MetaCommand::Help;
        let open = MetaCommand::Open(".open database.db".to_string());
        let unknown = MetaCommand::Unknown;

        assert_eq!(format!("{exit}"), ".exit");
        assert_eq!(format!("{help}"), ".help");
        assert_eq!(format!("{open}"), ".open");
        assert_eq!(format!("{unknown}"), "Unknown command");
    }
}
