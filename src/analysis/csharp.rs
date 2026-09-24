//! An analyzer for the C# / .NET toolchain (`dotnet build`, `dotnet test`,
//! `msbuild`, `csc`), whose diagnostics all follow the canonical MSBuild
//! format:
//!
//! ```text
//! Program.cs(12,30): error CS1002: ; expected [/path/to/proj.csproj]
//! /src/Lib.cs(7,13): warning CS0219: The variable 'x' is assigned but its value is never used [/path/to/proj.csproj]
//! MSBUILD : error MSB1009: Project file does not exist.
//! ```

use {
    super::*,
    crate::*,
    anyhow::Result,
    lazy_regex::*,
};

#[derive(Debug, Default)]
pub struct CsharpAnalyzer {
    lines: Vec<CommandOutputLine>,
}

#[derive(Debug)]
struct Diagnostic {
    kind: Kind,
    /// `path:line:col`, or `None` when the diagnostic isn't located
    /// (eg a global MSBuild error)
    location: Option<String>,
    /// eg `CS1002: ; expected`
    message: String,
}

impl Analyzer for CsharpAnalyzer {
    fn start(
        &mut self,
        _: &Mission,
    ) {
        self.lines.clear();
    }

    fn receive_line(
        &mut self,
        line: CommandOutputLine,
        command_output: &mut CommandOutput,
    ) {
        self.lines.push(line.clone());
        command_output.push(line);
    }

    fn build_report(&mut self) -> Result<Report> {
        Ok(build_report(&self.lines))
    }
}

/// Recognize a located diagnostic, eg
/// `Program.cs(12,30): error CS1002: ; expected [/path/to/proj.csproj]`
fn recognize_located(raw: &str) -> Option<Diagnostic> {
    let (_, path, line, col, level, message) = regex_captures!(
        r"^\s*(\S[^(]*)\((\d+)(?:,(\d+))?(?:,\d+,\d+)?\)\s*:\s*(error|warning)\s+(.+?)\s*$",
        raw
    )?;
    let message = strip_project_suffix(message);
    if message.is_empty() {
        return None;
    }
    let location = if col.is_empty() {
        format!("{path}:{line}")
    } else {
        format!("{path}:{line}:{col}")
    };
    Some(Diagnostic {
        kind: level_kind(level),
        location: Some(location),
        message,
    })
}

/// Recognize a diagnostic without source location, eg
/// `MSBUILD : error MSB1009: Project file does not exist.`
fn recognize_unlocated(raw: &str) -> Option<Diagnostic> {
    let (_, level, message) = regex_captures!(
        r"^\s*\S[^:]*\s:\s*(error|warning)\s+([A-Z]+\d+\s*:.+?)\s*$",
        raw
    )?;
    let message = strip_project_suffix(message);
    if message.is_empty() {
        return None;
    }
    Some(Diagnostic {
        kind: level_kind(level),
        location: None,
        message,
    })
}

fn level_kind(level: &str) -> Kind {
    if level == "warning" {
        Kind::Warning
    } else {
        Kind::Error
    }
}

/// Remove the trailing `[/path/to/project.csproj]` that MSBuild appends,
/// as it's noise repeated on every single diagnostic.
fn strip_project_suffix(message: &str) -> String {
    regex_replace!(
        r"\s*\[[^\[\]]*\.(?:cs|fs|vb|sln|msbuild)proj(?:\s*::[^\[\]]*)?\]\s*$"i,
        message,
        ""
    )
    .trim_end()
    .to_string()
}

fn recognize_diagnostic(tline: &TLine) -> Option<Diagnostic> {
    let raw = tline.to_raw();
    recognize_located(&raw).or_else(|| recognize_unlocated(&raw))
}

/// Build a report from the output of `dotnet build` and friends
pub fn build_report(cmd_lines: &[CommandOutputLine]) -> Report {
    let mut items = ItemAccumulator::default();
    let mut last_is_blank = true;
    for cmd_line in cmd_lines {
        if let Some(diag) = recognize_diagnostic(&cmd_line.content) {
            let title = match diag.kind {
                Kind::Warning => burp::warning_line_ts(&[TString::new("", diag.message)]),
                _ => burp::error_line(&diag.message),
            };
            items.start_item(diag.kind);
            items.push_line(LineType::Title(diag.kind), title);
            if let Some(location) = diag.location {
                items.push_line(LineType::Location, burp::location_line(location));
            }
            last_is_blank = false;
        } else {
            let is_blank = cmd_line.content.is_blank();
            if !(is_blank && last_is_blank) {
                items.push_line(LineType::Normal, cmd_line.content.clone());
            }
            last_is_blank = is_blank;
        }
    }
    items.report()
}

#[cfg(test)]
mod csharp_analyzer_tests {
    use super::*;

    fn diag(raw: &str) -> Option<Diagnostic> {
        recognize_diagnostic(&TLine::from_raw(raw.to_string()))
    }

    #[test]
    fn test_recognize_error_with_project_suffix() {
        let d = diag(
            r"/home/dev/app/Program.cs(12,30): error CS1002: ; expected [/home/dev/app/app.csproj]",
        )
        .unwrap();
        assert_eq!(d.kind, Kind::Error);
        assert_eq!(
            d.location.as_deref(),
            Some("/home/dev/app/Program.cs:12:30")
        );
        assert_eq!(d.message, "CS1002: ; expected");
    }

    #[test]
    fn test_recognize_warning_windows_path() {
        let d = diag(
            r"C:\src\app\Lib.cs(7,13): warning CS0219: The variable 'x' is assigned but its value is never used [C:\src\app\app.csproj]",
        )
        .unwrap();
        assert_eq!(d.kind, Kind::Warning);
        assert_eq!(d.location.as_deref(), Some(r"C:\src\app\Lib.cs:7:13"));
        assert_eq!(
            d.message,
            "CS0219: The variable 'x' is assigned but its value is never used"
        );
    }

    #[test]
    fn test_recognize_line_only_location() {
        let d = diag("Startup.cs(3): error CS0246: type not found").unwrap();
        assert_eq!(d.location.as_deref(), Some("Startup.cs:3"));
    }

    #[test]
    fn test_recognize_unlocated_msbuild_error() {
        let d = diag("MSBUILD : error MSB1009: Project file does not exist.").unwrap();
        assert_eq!(d.kind, Kind::Error);
        assert_eq!(d.location, None);
        assert_eq!(d.message, "MSB1009: Project file does not exist.");
    }

    #[test]
    fn test_ignore_normal_lines() {
        assert!(diag("  Determining projects to restore...").is_none());
        assert!(diag("Build succeeded.").is_none());
        assert!(diag("    0 Warning(s)").is_none());
        assert!(diag("Time Elapsed 00:00:01.23").is_none());
    }
}
