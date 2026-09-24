use {
    crate::*,
    lazy_regex::*,
};

/// Whether the CSI is the one rustc uses for the body of a diagnostic title,
/// ie what follows `error` or `warning`
#[cfg(not(windows))]
fn is_error_body(csi: &str) -> bool {
    csi == CSI_BOLD
}
#[cfg(windows)]
fn is_error_body(csi: &str) -> bool {
    csi == CSI_BOLD_WHITE || csi == CSI_BOLD_4BIT_WHITE
}

#[derive(Debug, Default)]
pub struct StandardLineAnalyzer;

impl LineAnalyzer for StandardLineAnalyzer {
    fn analyze_line(
        &mut self,
        cmd_line: &CommandOutputLine,
    ) -> LineAnalysis {
        analyze_line(cmd_line)
    }
}

fn analyze_line(cmd_line: &CommandOutputLine) -> LineAnalysis {
    let content = &cmd_line.content;
    let mut key = None;
    let line_type = if cmd_line.content.is_blank() {
        LineType::Normal
    } else if let Some(content) = cmd_line.content.if_unstyled() {
        if let Some((k, r)) = as_test_result(content) {
            key = Some(k.to_string());
            LineType::TestResult(r)
        } else if let Some(k) = as_test_stdout_title(content) {
            key = Some(k.to_string());
            LineType::Title(Kind::TestOutput)
        } else if let Some(k) = as_stack_overflow_message(content) {
            key = Some(k.to_string());
            LineType::Title(Kind::TestFail)
        } else if cmd_line.origin == CommandStream::StdErr
            && regex_is_match!("^error: ", content)
            && !regex_is_match!("^error: aborting due to", content)
        {
            // This recognizes the error in case there's no styling (eg miri run, see issue #251).
            // If there proves to be too many false positives, this might be moved to an "unstyled"
            // analyzer dedicated to those cases when we can't get any styling information.
            LineType::Title(Kind::Error)
        } else if cmd_line.origin == CommandStream::StdErr
            && regex_is_match!("^warning: ", content)
            && !regex_is_match!(r"generated \d+ warnings?$", content)
        {
            // This recognizes the warning in case there's no styling (eg miri run, see issue #251)
            LineType::Title(Kind::Warning)
        } else if regex_is_match!("^failures:$", content) {
            // this isn't very discriminant...
            LineType::Title(Kind::Sum)
        } else if regex_is_match!("[Rr]un with (`)?RUST_BACKTRACE=", content) {
            LineType::BacktraceSuggestion
        } else if regex_is_match!(r#", [^:\s'"]+:\d+:\d+$"#, content) {
            // this kind of location comes up in test failures
            LineType::Location
        } else if regex_is_match!(r#"^\s+--> [^:\s'"]+:\d+:\d+$"#, content) {
            // this comes up in test failures to compile
            LineType::Location
        } else if regex_is_match!(
            r#"^thread '.+'( \(\d+\))? panicked at [^:\s'"]+:\d+:\d+:$"#,
            content
        ) {
            // this comes up in test failures
            LineType::Location
        } else {
            LineType::Normal
        }
    } else {
        let ts0 = content.strings.get(0);
        let ts1 = content.strings.get(1);
        match (ts0, ts1) {
            (Some(title), Some(body)) => {
                match (
                    title.csi.as_ref(),
                    title.raw.as_ref(),
                    body.csi.as_ref(),
                    body.raw.as_ref(),
                ) {
                    (CSI_BOLD_RED | CSI_BOLD_4BIT_RED, "error", body_csi, body_raw)
                        if is_error_body(body_csi) && body_raw.starts_with(": aborting due to") =>
                    {
                        LineType::Title(Kind::Sum)
                    }
                    (CSI_BOLD_RED | CSI_BOLD_4BIT_RED, title_raw, body_csi, _)
                        if title_raw.starts_with("error") && is_error_body(body_csi) =>
                    {
                        LineType::Title(Kind::Error)
                    }
                    #[cfg(not(windows))]
                    (CSI_BOLD_YELLOW, "warning", _, body_raw) => {
                        determine_warning_type(body_raw, content)
                    }
                    #[cfg(windows)]
                    (
                        CSI_BOLD_YELLOW | CSI_BOLD_4BIT_YELLOW | CSI_BOLD_4BIT_BRIGHT_YELLOW,
                        "warning",
                        _,
                        body_raw,
                    ) => determine_warning_type(body_raw, content),
                    (
                        "",
                        title_raw,
                        CSI_BOLD_BLUE | CSI_BOLD_4BIT_BLUE | CSI_BOLD_4BIT_CYAN,
                        "--> ",
                    ) if is_spaces(title_raw) => LineType::Location,
                    (
                        "",
                        title_raw,
                        CSI_BOLD_BLUE | CSI_BOLD_4BIT_BLUE | CSI_BOLD_4BIT_CYAN,
                        "::: ",
                    ) if is_spaces(title_raw) => LineType::Location,
                    ("", k, CSI_BOLD_RED | CSI_RED, "FAILED") if content.strings.len() == 2 => {
                        if let Some(k) = as_test_name(k) {
                            key = Some(k.to_string());
                            LineType::TestResult(false)
                        } else {
                            LineType::Normal
                        }
                    }
                    ("", k, CSI_GREEN, "ok") => {
                        if let Some(k) = as_test_name(k) {
                            key = Some(k.to_string());
                            LineType::TestResult(true)
                        } else {
                            LineType::Normal
                        }
                    }
                    _ => LineType::Normal,
                }
            }
            (Some(content), None) => {
                if regex_is_match!(
                    r#"^\s*thread '.+'( \(\d+\))? panicked at [^:\s'"]+:\d+:\d+:$"#,
                    &content.raw
                ) {
                    // this comes up in nextest failures
                    LineType::Location
                } else {
                    LineType::Normal
                }
            }
            _ => LineType::Normal, // empty line
        }
    };
    LineAnalysis { line_type, key }
}

fn determine_warning_type(
    body_raw: &str,
    content: &TLine,
) -> LineType {
    if is_n_warnings_emitted(body_raw)
        || is_generated_n_warnings(&content.strings)
        || is_build_failed(content.strings.get(2))
    {
        LineType::Title(Kind::Sum)
    } else {
        LineType::Title(Kind::Warning)
    }
}

fn is_spaces(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_whitespace())
}

/// check if the string starts with something like ": 15 warnings emitted"
fn is_n_warnings_emitted(s: &str) -> bool {
    regex_is_match!(r#"^: \d+ warnings? emitted"#, s)
}
fn is_generated_n_warnings(ts: &[TString]) -> bool {
    ts.iter()
        .any(|ts| regex_is_match!(r#"generated \d+ warnings?"#, &ts.raw))
}
fn is_build_failed(ts: Option<&TString>) -> bool {
    ts.is_some_and(|ts| regex_is_match!(r#"^\s*build failed"#, &ts.raw))
}

/// similar to `as_test_result` but without the FAILED|ok part
/// This is used in case of styled output (because the FAILED|ok
/// part is in another `TString`)
fn as_test_name(s: &str) -> Option<&str> {
    regex_captures!(
        r#"^(?:test\s+)?(.+?)(?: - should panic\s*)?(?: - compile\s*)?\s+...\s*$"#,
        s
    )
    .map(|(_, key)| key)
}

/// return Some when the line is the non detailed
/// result of a test, for example
///
///  `"test str_fit::fitting_count_tests::test_count_fitting ... FAILED"`
/// or
///  `"test wrap::wrap_tests::check_space_removing ... ok"`
/// or
///  `"test src/lib.rs - (line 6) ... FAILED"`
/// or
///  `"test src/lib.rs - (line 10) ... ok"`
/// or
///  `"test src/mode.rs - mode::Mode::new (line 121) - compile ... FAILED"`
/// (in this case, the " - compile" part isn't in the key, see #64)
/// or
///  `"test tests::another - should panic ... FAILED"`
/// (in this case, the " - should panic" part isn't in the key, see #95)
fn as_test_result(s: &str) -> Option<(&str, bool)> {
    regex_captures!(
        r#"^\s*(?:test\s+)?(.+?)(?: - should panic\s*)?(?: - compile\s*)?\s+...\s+(\w+)$"#,
        s
    )
    .and_then(|(_, key, outcome)| match outcome {
        "ok" => Some((key, true)),
        "FAILED" => Some((key, false)),
        other => {
            warn!("unrecognized doctest outcome: {other:?}");
            None
        }
    })
}

/// return Some(key) when the line is like this:
/// "---- key stdout ----"
fn as_test_stdout_title(s: &str) -> Option<&str> {
    regex_captures!(r#"^---- (.+) stdout ----$"#, s).map(|(_, key)| key)
}

/// Returns Some(key) when the line is like this:
/// thread 'key' has overflowed its stack
fn as_stack_overflow_message(s: &str) -> Option<&str> {
    regex_captures!("^thread '(.+)' has overflowed its stack$", s).map(|(_, key)| key)
}

#[cfg(test)]
mod standard_line_analyzer_tests {
    use super::*;

    fn line_type(
        raw: &str,
        origin: CommandStream,
    ) -> LineType {
        let cmd_line = CommandOutputLine {
            content: TLine::from_tty(raw),
            origin,
        };
        analyze_line(&cmd_line).line_type
    }

    /// Output of `cargo check` on Windows, captured from a real run: rustc
    /// hasn't got the 256 colors there and falls back to the bright 4 bit
    /// ones, `93` for the warning, `97` for the body, `96` for the arrow.
    ///
    /// Before those were recognized, nothing at all was analyzed on Windows:
    /// no title, no location, hence no exported location either.
    #[cfg(windows)]
    #[test]
    fn recognize_bright_4bit_rustc_diagnostics() {
        assert_eq!(
            line_type(
                "\u{1b}[1m\u{1b}[93mwarning\u{1b}[0m\u{1b}[1m\u{1b}[97m: unused variable: `x`\u{1b}[0m",
                CommandStream::StdErr,
            ),
            LineType::Title(Kind::Warning),
        );
        assert_eq!(
            line_type(
                "\u{1b}[1m\u{1b}[91merror\u{1b}[0m\u{1b}[1m\u{1b}[97m: mismatched types\u{1b}[0m",
                CommandStream::StdErr,
            ),
            LineType::Title(Kind::Error),
        );
        assert_eq!(
            line_type(
                " \u{1b}[1m\u{1b}[96m--> \u{1b}[0msrc\\main.rs:1:17",
                CommandStream::StdErr,
            ),
            LineType::Location,
        );
    }

    /// The 256 colors variants, used when the terminal supports them, must
    /// keep being recognized.
    #[test]
    fn recognize_256_colors_rustc_diagnostics() {
        let arrow = format!(" {CSI_BOLD_BLUE}--> {CSI_RESET}src/main.rs:1:17");
        assert_eq!(line_type(&arrow, CommandStream::StdErr), LineType::Location);
        let arrow = format!(" {CSI_BOLD_4BIT_BLUE}--> {CSI_RESET}src/main.rs:1:17");
        assert_eq!(line_type(&arrow, CommandStream::StdErr), LineType::Location);
    }

    /// A summary line must not be taken for an error.
    #[test]
    fn aborting_summary_isnt_an_error() {
        #[cfg(windows)]
        let body = CSI_BOLD_4BIT_WHITE;
        #[cfg(not(windows))]
        let body = CSI_BOLD;
        let raw = format!(
            "{CSI_BOLD_4BIT_RED}error{CSI_RESET}{body}: aborting due to 1 previous error{CSI_RESET}"
        );
        assert_eq!(
            line_type(&raw, CommandStream::StdErr),
            LineType::Title(Kind::Sum),
        );
    }
}
