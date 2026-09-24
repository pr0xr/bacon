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
    rustc_hash::FxHashSet,
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

/// Build a report from the output of `dotnet build` and friends.
///
/// MSBuild prints every diagnostic twice: once as the compiler emits it, and
/// once more in the `Build FAILED.` / `Build succeeded.` summary. There is no
/// console logger option that suppresses the repeat (neither `NoSummary` nor a
/// lower verbosity does), and multi-targeted projects repeat diagnostics once
/// per target framework too. So identical diagnostics are folded into one item.
pub fn build_report(cmd_lines: &[CommandOutputLine]) -> Report {
    let mut items = ItemAccumulator::default();
    let mut last_is_blank = true;
    let mut seen: FxHashSet<String> = FxHashSet::default();
    for cmd_line in cmd_lines {
        if let Some(diag) = recognize_diagnostic(&cmd_line.content) {
            let key = format!(
                "{:?}\u{1}{}\u{1}{}",
                diag.kind,
                diag.location.as_deref().unwrap_or_default(),
                diag.message,
            );
            if !seen.insert(key) {
                // a repeat of an already reported diagnostic: skip it, and make
                // sure the summary noise following it isn't glued to some item
                items.close_item();
                last_is_blank = false;
                continue;
            }
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

    /// Verbatim output of `dotnet build --nologo` (SDK 10.0.401) on a console
    /// project with one unused local and one undefined symbol. Note that both
    /// diagnostics appear twice: inline, then again in the summary.
    const REAL_DOTNET_BUILD_OUTPUT: &str = r"  Determining projects to restore...
  All projects are up-to-date for restore.
C:\tmp\cstest\Program.cs(6,34): error CS0103: The name 'missingThing' does not exist in the current context [C:\tmp\cstest\cstest.csproj]
C:\tmp\cstest\Program.cs(5,13): warning CS0219: The variable 'unused' is assigned but its value is never used [C:\tmp\cstest\cstest.csproj]

Build FAILED.

C:\tmp\cstest\Program.cs(5,13): warning CS0219: The variable 'unused' is assigned but its value is never used [C:\tmp\cstest\cstest.csproj]
C:\tmp\cstest\Program.cs(6,34): error CS0103: The name 'missingThing' does not exist in the current context [C:\tmp\cstest\cstest.csproj]
    1 Warning(s)
    1 Error(s)

Time Elapsed 00:00:03.45";

    fn report_of(output: &str) -> Report {
        let lines = output
            .lines()
            .map(|l| CommandOutputLine {
                content: TLine::from_raw(l.to_string()),
                origin: CommandStream::StdOut,
            })
            .collect::<Vec<_>>();
        build_report(&lines)
    }

    #[test]
    fn test_real_dotnet_build_output_stats() {
        let report = report_of(REAL_DOTNET_BUILD_OUTPUT);
        // the duplicated summary must not double the counts
        assert_eq!(report.stats.errors, 1);
        assert_eq!(report.stats.warnings, 1);
    }

    #[test]
    fn test_real_dotnet_build_output_titles_and_locations() {
        let report = report_of(REAL_DOTNET_BUILD_OUTPUT);
        let rendered: Vec<String> = report
            .lines
            .iter()
            .map(|line| line.content.to_raw())
            .collect();
        assert!(
            rendered.iter().any(|l| l
                == "error: CS0103: The name 'missingThing' does not exist in the current context"),
            "missing error title in {rendered:#?}"
        );
        assert!(
            rendered
                .iter()
                .any(|l| l == r"   --> C:\tmp\cstest\Program.cs:6:34"),
            "missing error location in {rendered:#?}"
        );
        assert!(
            rendered.iter().any(|l| l
                == "warning: CS0219: The variable 'unused' is assigned but its value is never used"),
            "missing warning title in {rendered:#?}"
        );
        assert!(
            rendered
                .iter()
                .any(|l| l == r"   --> C:\tmp\cstest\Program.cs:5:13"),
            "missing warning location in {rendered:#?}"
        );
    }

    /// A multi-targeted project emits the same diagnostic once per TFM.
    #[test]
    fn test_multi_target_duplicates_are_folded() {
        let report = report_of(
            r"Lib.cs(3,9): warning CS0219: unused [C:\a\a.csproj::TargetFramework=net8.0]
Lib.cs(3,9): warning CS0219: unused [C:\a\a.csproj::TargetFramework=net9.0]",
        );
        assert_eq!(report.stats.warnings, 1);
    }
}
