extern crate clap;
mod repl;

use repl::{REPLHelper, get_config};
use repl::metacommand::*;

use rustyline::error::ReadlineError;
use rustyline::{Editor};

use clap::{App, crate_authors, crate_description, crate_version};

use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::Parser;

fn main() -> rustyline::Result<()> {
    env_logger::init();

    let _matches = App::new("Rust-SQLite")
                          .version(crate_version!())
                          .author(crate_authors!())
                          .about(crate_description!())
                          .get_matches();

    // Starting Rustyline with a default configuration
    let config = get_config();

    // Getting a new Rustyline Helper
    let helper = REPLHelper::default();
    
    // Initiatlizing Rustyline Editor with set config and setting helper
    let mut repl = Editor::with_config(config);
    repl.set_helper(Some(helper));

    // This method loads history file into memory
    // If it doesn't exist, creates one
    // TODO: Check history file size and if too big, clean it.
    if repl.load_history("history").is_err() {
        println!("No previous history.");
    }

    // Friendly intro message for the user
    println!("Rust-SQLite - {}\n{}{}{}{}",
    crate_version!(),
    "Enter .exit to quit.\n",
    "Enter .help for usage hints.\n",
    "Connected to a transient in-memory database.\n",
    "Use '.open FILENAME' to reopen on a persistent database.");

    let dialect = SQLiteDialect{};

    // Counter is set to improve user experience and show user how many 
    // commands he has ran.
    let mut count = 1;
    loop {
        let p = format!("rust-sqlite | {}> ", count);
        repl.helper_mut()
            .expect("No helper found")
            .colored_prompt = format!("\x1b[1;32m{}\x1b[0m", p);
        // Source for ANSI Color information: http://www.perpetualpc.net/6429_colors.html#color_list
        // http://bixense.com/clicolors/

        let readline = repl.readline(&p);
        match readline {
            Ok(command) => {
                repl.add_history_entry(command.as_str());
                if command.starts_with(".") {
                    let action = get_meta_command(&command);
                    match action {
                        Some(response) => println!("{}",response),
                        None => break,
                    }
                }else{
                    let ast = Parser::parse_sql(&dialect, &command).unwrap();
                    println!("AST: {:?}", ast);
                }
            }
            Err(ReadlineError::Interrupted) => {
                break;
            }
            Err(ReadlineError::Eof) => {
                break;
            }
            Err(err) => {
                println!("Error: {:?}", err);
                break;
            }
        }
        count += 1;
    }
    repl.append_history("history").unwrap();

    Ok(())
}