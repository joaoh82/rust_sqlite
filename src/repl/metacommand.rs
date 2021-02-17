// Used to implement the Display trait
use std::fmt;

/// MetaCommand enumeration
#[derive(Debug, PartialEq)]
pub enum MetaCommand {
    Exit,
    Help,
    Open,
}

/// Trait responsible for translating type into a formated text.
impl fmt::Display for MetaCommand {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            MetaCommand::Exit => f.write_str(".exit"),
            MetaCommand::Help => f.write_str(".help"),
            MetaCommand::Open => f.write_str(".open"),
        }
    }
}

/// MetaCommandResult enumeration
#[derive(Debug, PartialEq)]
pub enum MetaCommandResult {
    MetaCommandSuccess(MetaCommand),
    MetaCommandUnrecognizedCommand,
}

/// Checks if meta command exists and returns Enum type or MetaCommandResult::MetaCommandUnrecognizedCommand 
fn check_meta_command(command: &String) -> MetaCommandResult {
    if command.eq(".exit") {
        MetaCommandResult::MetaCommandSuccess(MetaCommand::Exit)
    } else if command.eq(".help") {
        MetaCommandResult::MetaCommandSuccess(MetaCommand::Help)
    } else if command.eq(".open") {
        MetaCommandResult::MetaCommandSuccess(MetaCommand::Open)
    } else {
        MetaCommandResult::MetaCommandUnrecognizedCommand 
    }
}

/// Function responsible for calling any action regarding a command and also returning an Option<String> to be 
/// printed out to the user
pub fn get_meta_command(command: &String) -> Option<String> {
    let meta_command = check_meta_command(&command);
    match meta_command {
        MetaCommandResult::MetaCommandSuccess(cmd) => {
            match cmd {
                MetaCommand::Exit => None,
                MetaCommand::Help => {
                    Some(format!("{}{}{}{}","Special commands:\n",
                            ".help - Display this message\n",
                            ".open <FILENAME> - Reopens a persistent database.\n",
                            ".exit - Quits this application"))
                },
                MetaCommand::Open => Some(format!("To be implemented"))
            }
        },
        MetaCommandResult::MetaCommandUnrecognizedCommand => {
            Some(format!("Error: unknown command or invalid arguments: '{}'. Enter '.help'", &command))
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_meta_command_exit_test() {
        let inputed_command = String::from(".exit");

        let function_result = get_meta_command(&inputed_command);
        assert_eq!(function_result, None);
    }

    #[test]
    fn get_meta_command_help_test() {
        let inputed_command = String::from(".help");

        let function_result = get_meta_command(&inputed_command);
        assert_eq!(function_result.is_some(), true);
    }

    #[test]
    fn get_meta_command_open_test() {
        let inputed_command = String::from(".open");

        let function_result = get_meta_command(&inputed_command);
        assert_eq!(function_result.is_some(), true);
    }

    #[test]
    fn get_meta_command_unknown_command_test() {
        let inputed_command = String::from(".random_command");

        let function_result = get_meta_command(&inputed_command);
        assert_eq!(function_result.is_some(), true);
    }

    #[test]
    fn check_meta_command_success_test() {
        let inputed_command = String::from(".exit");

        let function_result = check_meta_command(&inputed_command);
        assert_eq!(function_result, MetaCommandResult::MetaCommandSuccess(MetaCommand::Exit));
    }

    #[test]
    fn check_meta_command_failed_test() {
        let inputed_command = String::from(".random_command");

        let function_result = check_meta_command(&inputed_command);
        assert_eq!(function_result, MetaCommandResult::MetaCommandUnrecognizedCommand);
    }

    #[test]
    fn meta_command_display_trait_test() {
        let exit = MetaCommand::Exit;
        let help = MetaCommand::Help;
        let open = MetaCommand::Open;

        assert_eq!(format!("{}", exit), ".exit");
        assert_eq!(format!("{}", help), ".help");
        assert_eq!(format!("{}", open), ".open");
    }
}