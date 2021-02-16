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
pub fn check_meta_command(command: &String) -> MetaCommandResult {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}