//! The input session of the REPL.

use std::io;

use crate::message::filler::Prompter;

use super::prompt::InteractivePrompter;

/// The input of the REPL. It reads the command lines and asks the user for values.
pub trait Session {
    /// Reads a command line. `Ok(None)` means the input is over.
    fn read_line(&mut self, prompt: &str) -> io::Result<Option<String>>;

    /// Creates a prompter that asks the user for the values of a request.
    fn prompter(&mut self) -> Box<dyn Prompter>;
}

/// A session that uses reedline and dialoguer.
pub struct TerminalSession {
    reedline: reedline::Reedline,
    colored: bool,
}

impl TerminalSession {
    /// Creates a new session. The history remembers at most `history_size` entries.
    pub fn new(
        completer: Box<dyn reedline::Completer + Send>,
        history_size: usize,
        colored: bool,
    ) -> Result<Self, reedline::ReedlineError> {
        let history = reedline::FileBackedHistory::new(history_size)?;
        let reedline = reedline::Reedline::create()
            .with_completer(completer)
            .with_history(Box::new(history));
        Ok(Self { reedline, colored })
    }
}

impl Session for TerminalSession {
    fn read_line(&mut self, prompt: &str) -> io::Result<Option<String>> {
        let prompt = super::ReplPrompt::new(prompt.to_string());
        match self.reedline.read_line(&prompt) {
            Ok(reedline::Signal::Success(line)) => Ok(Some(line)),
            Ok(reedline::Signal::CtrlD) => Ok(None),
            Ok(_) => Ok(Some(String::new())),
            Err(err) => Err(err),
        }
    }

    fn prompter(&mut self) -> Box<dyn Prompter> {
        Box::new(InteractivePrompter::new(self.colored))
    }
}
