//! The input session of the REPL.

use std::io;

use reedline::{
    ColumnarMenu, Emacs, KeyCode, KeyModifiers, Keybindings, MenuBuilder, ReedlineEvent,
    ReedlineMenu,
};

use crate::message::filler::Prompter;

use super::prompt::InteractivePrompter;

/// The name of the menu that shows the completion candidates.
const COMPLETION_MENU: &str = "completion_menu";

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
        let menu = ColumnarMenu::default().with_name(COMPLETION_MENU);
        let reedline = reedline::Reedline::create()
            .with_completer(completer)
            .with_menu(ReedlineMenu::EngineCompleter(Box::new(menu)))
            .with_edit_mode(Box::new(Emacs::new(keybindings())))
            .with_history(Box::new(history));
        Ok(Self { reedline, colored })
    }
}

/// The keybindings of the REPL.
///
/// They extend the default emacs ones with the keys that open the completion menu.
/// Reedline binds no key to the completion by default, so without them the
/// completer is never called.
///
/// TODO: the original shows the candidates as soon as they are typed, because its
/// line editor renders them below the prompt. Reedline only queries the completer
/// when a key is pressed, so the candidates appear on Tab. Rendering them live would
/// need a custom prompt that paints the candidates below the input.
fn keybindings() -> Keybindings {
    let mut keybindings = reedline::default_emacs_keybindings();

    // Tab opens the completion menu and cycles through the candidates.
    keybindings.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu(COMPLETION_MENU.to_string()),
            ReedlineEvent::MenuNext,
        ]),
    );

    // Shift+Tab cycles through the candidates backwards. The terminal reports it
    // either as BackTab with the shift modifier or as BackTab alone.
    for modifiers in [KeyModifiers::SHIFT, KeyModifiers::NONE] {
        keybindings.add_binding(
            modifiers,
            KeyCode::BackTab,
            ReedlineEvent::UntilFound(vec![
                ReedlineEvent::Menu(COMPLETION_MENU.to_string()),
                ReedlineEvent::MenuPrevious,
            ]),
        );
    }

    // Ctrl+Space opens the menu as well, for the terminals where Tab is taken.
    keybindings.add_binding(
        KeyModifiers::CONTROL,
        KeyCode::Char(' '),
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu(COMPLETION_MENU.to_string()),
            ReedlineEvent::MenuNext,
        ]),
    );

    keybindings
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
