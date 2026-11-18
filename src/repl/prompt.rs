//! The interactive prompt of the REPL, bridging reedline and dialoguer to the
//! `Prompter` trait of the message filler.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use dialoguer::Select;
use dialoguer::theme::{ColorfulTheme, SimpleTheme};
use reedline::{
    ColumnarMenu, KeyCode, KeyModifiers, Keybindings, MenuBuilder, Prompt, ReedlineEvent, Signal,
};

use crate::message::filler::{FillError, Prompter};

/// The name of the menu that shows the completion candidates.
pub const COMPLETION_MENU: &str = "completion_menu";

/// The keybindings of the line editor.
///
/// They extend the default emacs ones with the keys that open the completion menu.
/// Reedline binds no key to the completion by default, so without them the
/// completer is never called.
///
/// TODO: the original shows the candidates as soon as they are typed, because its
/// line editor renders them below the prompt. Reedline only queries the completer
/// when a key is pressed, so the candidates appear on Tab. Rendering them live would
/// need a custom prompt that paints the candidates below the input.
pub fn keybindings() -> Keybindings {
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

/// The menu that shows the completion candidates.
pub fn completion_menu() -> Box<dyn reedline::Menu> {
    Box::new(ColumnarMenu::default().with_name(COMPLETION_MENU))
}

/// The splash text of the REPL.
pub const SPLASH_TEXT: &str = r"
  ______
 |  ____|
 | |__    __   __   __ _   _ __    ___
 |  __|   \ \ / /  / _. | | '_ \  / __|
 | |____   \ V /  | (_| | | | | | \__ \
 |______|   \_/    \__,_| |_| |_| |___|

 more expressive universal gRPC client

";

/// A prompt that renders a fixed prefix.
#[derive(Debug, Clone)]
pub struct InputPrompt {
    prefix: String,
}

impl InputPrompt {
    /// Creates a new prompt.
    pub fn new(prefix: String) -> Self {
        Self { prefix }
    }
}

impl Prompt for InputPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.prefix)
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, _mode: reedline::PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("::: ")
    }

    fn render_prompt_history_search_indicator(
        &self,
        _history_search: reedline::PromptHistorySearch,
    ) -> Cow<'_, str> {
        Cow::Borrowed("(reverse-i-search) ")
    }
}

/// A prompter that asks the user for values with reedline and dialoguer.
pub struct InteractivePrompter {
    editor: reedline::Reedline,
    colored: bool,
    /// The completer of the paths, which is enabled while a path is typed.
    ///
    /// The line editor takes its completer at the creation, so the completer of the
    /// paths is installed through a delegate that is enabled only when it is needed.
    path_completer: Arc<Mutex<Option<Box<dyn reedline::Completer + Send>>>>,
}

impl InteractivePrompter {
    /// Creates a new prompter.
    pub fn new(colored: bool) -> Self {
        let path_completer: Arc<Mutex<Option<Box<dyn reedline::Completer + Send>>>> =
            Arc::new(Mutex::new(None));
        let editor = reedline::Reedline::create()
            .with_completer(Box::new(Delegate {
                active: path_completer.clone(),
            }))
            .with_menu(reedline::ReedlineMenu::EngineCompleter(completion_menu()))
            .with_edit_mode(Box::new(reedline::Emacs::new(keybindings())));

        Self {
            editor,
            colored,
            path_completer,
        }
    }
}

/// The completer of the line editor that delegates to the completer of the paths.
struct Delegate {
    /// The completer that is enabled, if there is one.
    active: Arc<Mutex<Option<Box<dyn reedline::Completer + Send>>>>,
}

impl reedline::Completer for Delegate {
    fn complete(&mut self, line: &str, pos: usize) -> reedline::CompletionResult {
        let mut active = self.active.lock().unwrap();
        match active.as_mut() {
            Some(completer) => completer.complete(line, pos),
            None => reedline::CompletionResult::fresh(Vec::new()),
        }
    }
}

/// The completer of the paths of the files.
struct PathCompleter;

impl reedline::Completer for PathCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> reedline::CompletionResult {
        let pos = pos.min(line.len());
        let Some(before) = line.get(..pos) else {
            return reedline::CompletionResult::fresh(Vec::new());
        };

        // The word before the cursor is the path that is completed.
        let start = before
            .rfind(char::is_whitespace)
            .map(|i| i + 1)
            .unwrap_or(0);
        let word = &before[start..];

        let suggestions: Vec<reedline::Suggestion> = path_candidates(word)
            .into_iter()
            .map(|(value, description)| reedline::Suggestion {
                value,
                description,
                span: reedline::Span::new(start, pos),
                ..Default::default()
            })
            .collect();

        reedline::CompletionResult::fresh(suggestions)
    }
}

/// Returns the paths that start with the given text.
///
/// The directories are suffixed with a slash, so that their contents are completed
/// when they are accepted.
fn path_candidates(text: &str) -> Vec<(String, Option<String>)> {
    let (dir, prefix) = match text.rfind('/') {
        Some(i) => (&text[..i], &text[i + 1..]),
        None => ("", text),
    };
    let dir = if dir.is_empty() { "." } else { dir };

    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut candidates: Vec<(String, Option<String>)> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            let display = if is_dir { format!("{name}/") } else { name };
            let value = if dir == "." {
                display
            } else {
                format!("{dir}/{display}")
            };
            let description = if is_dir {
                Some("directory".to_string())
            } else {
                Some("file".to_string())
            };
            (value, description)
        })
        .collect();
    candidates.sort();
    candidates
}

impl Prompter for InteractivePrompter {
    fn input(&mut self, prefix: &str) -> Result<String, FillError> {
        let prompt = InputPrompt::new(prefix.to_string());
        match self.editor.read_line(&prompt) {
            Ok(Signal::Success(line)) => Ok(line),
            // ctrl+c skips the rest of the fields.
            Ok(Signal::CtrlC(_)) => Err(FillError::Abort),
            // ctrl+d finishes the input.
            Ok(Signal::CtrlD) => Err(FillError::Eof),
            Ok(Signal::HostCommand(_)) | Ok(Signal::ExternalBreak(_)) => Err(FillError::Eof),
            Ok(_) => Err(FillError::Eof),
            Err(err) => Err(FillError::Io(format!("failed to read the input: {err}"))),
        }
    }

    fn input_path(&mut self, prefix: &str) -> Result<String, FillError> {
        // The completer of the paths is enabled only while the path is typed.
        *self.path_completer.lock().unwrap() = Some(Box::new(PathCompleter));
        let result = self.input(prefix);
        *self.path_completer.lock().unwrap() = None;
        result
    }

    fn select(&mut self, message: &str, options: &[String]) -> Result<usize, FillError> {
        // A canceled selection falls back to the default value, like the original does.
        Ok(self.select_opt(message, options)?.unwrap_or(0))
    }

    fn select_opt(
        &mut self,
        message: &str,
        options: &[String],
    ) -> Result<Option<usize>, FillError> {
        if options.is_empty() {
            return Err(FillError::Invalid("no options to select".to_string()));
        }

        let selection = if self.colored {
            Select::with_theme(&ColorfulTheme::default())
                .with_prompt(message)
                .default(0)
                .items(options)
                .interact_opt()
        } else {
            Select::with_theme(&SimpleTheme)
                .with_prompt(message)
                .default(0)
                .items(options)
                .interact_opt()
        }
        .map_err(|e| FillError::Invalid(e.to_string()))?;

        Ok(selection)
    }
}

impl Default for InteractivePrompter {
    fn default() -> Self {
        Self::new(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates a directory with a file and a subdirectory in it.
    fn fixture(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("evans-paths-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("file.bin"), b"").unwrap();
        std::fs::create_dir(dir.join("subdir")).unwrap();
        dir
    }

    #[test]
    fn completes_the_paths_of_the_files() {
        let dir = fixture("all");

        let candidates = path_candidates(&format!("{}/", dir.display()));
        let values: Vec<&str> = candidates.iter().map(|(value, _)| value.as_str()).collect();
        assert!(values.contains(&format!("{}/file.bin", dir.display()).as_str()));
        // The directories are suffixed with a slash, so that their contents are
        // completed when they are accepted.
        assert!(values.contains(&format!("{}/subdir/", dir.display()).as_str()));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn completes_the_paths_by_their_prefix() {
        let dir = fixture("prefix");

        let candidates = path_candidates(&format!("{}/fi", dir.display()));
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0, format!("{}/file.bin", dir.display()));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn completes_the_paths_of_the_current_directory() {
        let dir = fixture("cwd");
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();

        let candidates = path_candidates("");
        let values: Vec<&str> = candidates.iter().map(|(value, _)| value.as_str()).collect();
        assert!(values.contains(&"file.bin"));
        assert!(values.contains(&"subdir/"));

        std::env::set_current_dir(previous).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn has_no_candidates_for_an_unknown_directory() {
        assert!(path_candidates("/nonexistent/directory/").is_empty());
    }
}
