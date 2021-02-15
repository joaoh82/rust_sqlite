extern crate clap;
mod repl;

use repl::{REPLHelper, get_config};

use rustyline::error::ReadlineError;
use rustyline::{Editor};

use clap::{App, crate_version};

fn main() -> rustyline::Result<()> {
    env_logger::init();

    let matches = App::new("Rust-SQLite")
                          .version("0.0.1")
                          .author("João Henrique Machado Silva <joaoh82@gmail.com>")
                          .about("Light version of SQLite developed with Rust")
                          .get_matches();

    // Starting Rustyline with a default configuration
    let config = get_config();

    // Getting a new Rustyline Helper
    let helper = REPLHelper::new();
    
    // Initiatlizing Rustyline Editor with set config and setting helper
    let mut repl = Editor::with_config(config);
    repl.set_helper(Some(helper));

    // This method loads history file into memory
    // If it doesn't exist, creates one
    // TODO: Check history file size and if too big, clean it.
    if repl.load_history("history").is_err() {
        println!("No previous history.");
    }
    // Counter is set to improve user experience and show user how many 
    // commands he has ran.
    let mut count = 1;
    loop {
        if count == 1 {
            // Friendly intro message for the user
            println!("{}{}{}{}{}",
            format!("Rust-SQLite - {}\n", crate_version!()),
            "Enter .exit to quit.\n",
            "Enter .help for usage hints.\n",
            "Connected to a transient in-memory database.\n",
            "Use '.open FILENAME' to reopen on a persistent database.");
            //TODO: Get info about application name and version dinamically.
        }

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
                // println!("Command: {}", line);
                if command.eq(".exit") {
                    break;
                }else{
                    println!("Unrecognized command '{}'", &command);
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