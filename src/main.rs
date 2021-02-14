mod repl;

use repl::{REPLHelper, get_config};

use rustyline::error::ReadlineError;
use rustyline::{Editor};

fn main() -> rustyline::Result<()> {
    env_logger::init();

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
        let p = format!("rust-sqlite | {}> ", count);
        repl.helper_mut()
            .expect("No helper found")
            .colored_prompt = format!("\x1b[1;32m{}\x1b[0m", p);
        // Source for ANSI Color information: http://www.perpetualpc.net/6429_colors.html#color_list
        // http://bixense.com/clicolors/

        let readline = repl.readline(&p);
        match readline {
            Ok(line) => {
                repl.add_history_entry(line.as_str());
                // println!("Line: {}", line);
                if line.eq(".exit") {
                    break;
                }else{
                    println!("Unrecognized command '{}'", &line);
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