//! The interactive prompt of the REPL, bridging reedline and dialoguer to the
//! `Prompter` trait of the message filler.

use std::borrow::Cow;

use dialoguer::Select;
use dialoguer::theme::{ColorfulTheme, SimpleTheme};
use reedline::{Prompt, Signal};

use crate::message::filler::{FillError, Prompter};

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
}

impl InteractivePrompter {
    /// Creates a new prompter.
    pub fn new(colored: bool) -> Self {
        Self {
            editor: reedline::Reedline::create(),
            colored,
        }
    }
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
