//! An analyzer for the C# / .NET toolchain (`dotnet build`, `dotnet test`,
//! `msbuild`, `csc`).
//!
//! Compiler diagnostics follow the canonical MSBuild format:
//!
//! ```text
//! Program.cs(12,30): error CS1002: ; expected [/path/to/proj.csproj]
//! /src/Lib.cs(7,13): warning CS0219: The variable 'x' is assigned but its value is never used [/path/to/proj.csproj]
//! MSBUILD : error MSB1009: Project file does not exist.
//! ```
//!
//! `dotnet test` failures use an unrelated format, emitted by the VSTest
//! console logger (and therefore shared by xUnit, NUnit and MSTest):
//!
//! ```text
//!   Failed cstest.UnitTest1.FailingEqualityTest [1 ms]
//!   Error Message:
//!    Assert.Equal() Failure: Values differ
//!   Stack Trace:
//!      at cstest.UnitTest1.FailingEqualityTest() in /src/UnitTest1.cs:line 14
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

#[derive(Debug)]
struct StackFrame {
    /// true when the frame is in the test framework, not in the user's test
    is_framework: bool,
    /// `path:line`
    location: String,
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

/// Recognize the start of a `dotnet test` failure, eg
/// `  Failed cstest.UnitTest1.FailingEqualityTest [1 ms]`
///
/// Returns the test name. The leading whitespace is required, so that the
/// final `Failed!  - Failed: 3, Passed: 2, ...` recap isn't mistaken for one.
fn recognize_test_failure(raw: &str) -> Option<String> {
    let (_, name) = regex_captures!(r"^\s+Failed\s+(\S.*?)\s+\[[^\[\]]*\]\s*$", raw)?;
    Some(name.to_string())
}

/// Recognize a stack frame pointing into source, eg
/// `   at cstest.UnitTest1.FailingEqualityTest() in /src/UnitTest1.cs:line 14`
fn recognize_stack_frame(raw: &str) -> Option<StackFrame> {
    let (_, method, path, line) =
        regex_captures!(r"^\s+at\s+([^\s(]+).*?\sin\s(\S.*?):line\s+(\d+)\s*$", raw)?;
    Some(StackFrame {
        is_framework: is_framework_frame(method, path),
        location: format!("{path}:{line}"),
    })
}

/// Tell whether a frame belongs to the test framework rather than to the
/// user's test.
///
/// MSTest in particular puts several of its own frames on top of the trace
/// (`Assert.ThrowAssertAreEqualFailed`, `Assert.AreEqual`...), so taking the
/// topmost frame would send the user into the framework's sources. Those
/// frames are recognizable either by their namespace or by their `/_/` path,
/// which is what SourceLink produces for code built from a NuGet package.
fn is_framework_frame(
    method: &str,
    path: &str,
) -> bool {
    path.starts_with("/_/")
        || regex_is_match!(
            r"^(System\.|Microsoft\.VisualStudio\.TestTools\.|Microsoft\.TestPlatform\.|NUnit\.|Xunit\.|InvokeStub_)",
            method
        )
}

/// Recognize the final recap of a test run, eg
/// `Failed!  - Failed: 3, Passed: 2, Skipped: 0, Total: 5, Duration: 105 ms`
fn is_test_run_recap(raw: &str) -> bool {
    regex_is_match!(r"^\s*(Failed|Passed|Skipped)!\s+-\s+Failed:", raw)
}

/// Build a report from the output of `dotnet build` / `dotnet test` and friends.
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
    // set while inside a `dotnet test` failure whose location is still unknown
    let mut want_stack_location = false;
    for cmd_line in cmd_lines {
        let raw = cmd_line.content.to_raw();
        if let Some(test_name) = recognize_test_failure(&raw) {
            items.push_failure_title(burp::failure_line(&test_name));
            want_stack_location = true;
            last_is_blank = false;
            continue;
        }
        if want_stack_location
            && let Some(frame) = recognize_stack_frame(&raw)
            && !frame.is_framework
        {
            // the first frame in the user's own code: deeper frames, and the
            // framework frames above it, are noise
            items.push_line(LineType::Location, burp::location_line(frame.location));
            want_stack_location = false;
            last_is_blank = false;
            continue;
        }
        if is_test_run_recap(&raw) {
            items.close_item();
            want_stack_location = false;
            last_is_blank = false;
            continue;
        }
        if let Some(diag) = recognize_diagnostic(&cmd_line.content) {
            want_stack_location = false;
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

    /// Verbatim output of `dotnet test --nologo` (SDK 10.0.401, xUnit 2.9.3)
    /// on a project with 2 passing and 3 failing tests.
    const REAL_DOTNET_TEST_OUTPUT: &str = r"  Determining projects to restore...
  All projects are up-to-date for restore.
  cstest2 -> C:\tmp\cstest2\bin\Debug\net10.0\cstest2.dll
Test run for C:\tmp\cstest2\bin\Debug\net10.0\cstest2.dll (.NETCoreApp,Version=v10.0)
A total of 1 test files matched the specified pattern.
[xUnit.net 00:00:00.45]     cstest2.UnitTest1.FailingTheory(n: 1) [FAIL]
[xUnit.net 00:00:00.46]     cstest2.UnitTest1.FailingEqualityTest [FAIL]
[xUnit.net 00:00:00.46]     cstest2.UnitTest1.ThrowingTest [FAIL]
  Failed cstest2.UnitTest1.FailingTheory(n: 1) [< 1 ms]
  Error Message:
   n was 1
  Stack Trace:
     at cstest2.UnitTest1.FailingTheory(Int32 n) in C:\tmp\cstest2\UnitTest1.cs:line 28
   at InvokeStub_UnitTest1.FailingTheory(Object, Span`1)
   at System.Reflection.MethodBaseInvoker.InvokeWithOneArg(Object obj, BindingFlags invokeAttr, Binder binder, Object[] parameters, CultureInfo culture)
  Failed cstest2.UnitTest1.FailingEqualityTest [1 ms]
  Error Message:
   Assert.Equal() Failure: Values differ
Expected: 5
Actual:   4
  Stack Trace:
     at cstest2.UnitTest1.FailingEqualityTest() in C:\tmp\cstest2\UnitTest1.cs:line 14
   at System.Reflection.MethodBaseInvoker.InterpretedInvoke_Method(Object obj, IntPtr* args)
  Failed cstest2.UnitTest1.ThrowingTest [< 1 ms]
  Error Message:
   System.InvalidOperationException : boom
  Stack Trace:
     at cstest2.UnitTest1.ThrowingTest() in C:\tmp\cstest2\UnitTest1.cs:line 20
   at System.Reflection.MethodBaseInvoker.InterpretedInvoke_Method(Object obj, IntPtr* args)

Failed!  - Failed:     3, Passed:     2, Skipped:     0, Total:     5, Duration: 105 ms - cstest2.dll (net10.0)";

    #[test]
    fn test_real_dotnet_test_output_counts_failures() {
        let report = report_of(REAL_DOTNET_TEST_OUTPUT);
        assert_eq!(report.stats.test_fails, 3);
        assert_eq!(report.stats.errors, 0);
        assert_eq!(report.stats.warnings, 0);
    }

    #[test]
    fn test_real_dotnet_test_output_titles_and_locations() {
        let report = report_of(REAL_DOTNET_TEST_OUTPUT);
        let rendered: Vec<String> = report
            .lines
            .iter()
            .map(|line| line.content.to_raw())
            .collect();
        for expected in [
            "failure: cstest2.UnitTest1.FailingTheory(n: 1)",
            r"   --> C:\tmp\cstest2\UnitTest1.cs:28",
            "failure: cstest2.UnitTest1.FailingEqualityTest",
            r"   --> C:\tmp\cstest2\UnitTest1.cs:14",
            "failure: cstest2.UnitTest1.ThrowingTest",
            r"   --> C:\tmp\cstest2\UnitTest1.cs:20",
        ] {
            assert!(
                rendered.iter().any(|l| l == expected),
                "missing {expected:?} in {rendered:#?}"
            );
        }
    }

    #[test]
    fn test_test_failure_keeps_assertion_message() {
        let report = report_of(REAL_DOTNET_TEST_OUTPUT);
        let rendered: Vec<String> = report
            .lines
            .iter()
            .map(|line| line.content.to_raw())
            .collect();
        assert!(
            rendered
                .iter()
                .any(|l| l.contains("Assert.Equal() Failure: Values differ")),
            "assertion detail was dropped: {rendered:#?}"
        );
    }

    /// The final recap must not be taken for a failing test named `-`.
    #[test]
    fn test_run_recap_is_not_a_failure() {
        let report = report_of(
            "Failed!  - Failed:     3, Passed:     2, Skipped:     0, Total:     5, Duration: 105 ms - cstest2.dll (net10.0)",
        );
        assert_eq!(report.stats.test_fails, 0);
    }

    /// Only the first stack frame is kept: deeper frames are reflection noise.
    #[test]
    fn test_only_first_stack_frame_is_a_location() {
        let report = report_of(REAL_DOTNET_TEST_OUTPUT);
        let locations = report
            .lines
            .iter()
            .filter(|line| line.line_type == LineType::Location)
            .count();
        assert_eq!(locations, 3);
    }

    /// Verbatim `dotnet test` output for MSTest (SDK 10.0.401). MSTest stacks
    /// several of its own frames on top of the user's, and those frames have
    /// real `:line` info, so the naive "topmost frame" rule would point into
    /// MSTest's sources instead of the failing test.
    const REAL_MSTEST_OUTPUT: &str = r"  Failed FailingTest [19 ms]
  Error Message:
   Assert.AreEqual failed. Expected:<5>. Actual:<4>. 'expected' expression: '5', 'actual' expression: '2 + 2'.
  Stack Trace:
     at Microsoft.VisualStudio.TestTools.UnitTesting.Assert.ThrowAssertAreEqualFailed(Object expected, Object actual, String userMessage) in /_/src/TestFramework/TestFramework/Assertions/Assert.AreEqual.cs:line 665
   at Microsoft.VisualStudio.TestTools.UnitTesting.Assert.AreEqual[T](T expected, T actual, IEqualityComparer`1 comparer, String message, String expectedExpression, String actualExpression) in /_/src/TestFramework/TestFramework/Assertions/Assert.AreEqual.cs:line 492
   at cs_mstest.Test1.FailingTest() in C:\tmp\cs_mstest\Test1.cs:line 9
   at System.Reflection.MethodBaseInvoker.InterpretedInvoke_Method(Object obj, IntPtr* args)

Failed!  - Failed:     1, Passed:     0, Skipped:     0, Total:     1, Duration: 33 ms - cs_mstest.dll (net10.0)";

    #[test]
    fn test_mstest_location_skips_framework_frames() {
        let report = report_of(REAL_MSTEST_OUTPUT);
        assert_eq!(report.stats.test_fails, 1);
        let locations: Vec<String> = report
            .lines
            .iter()
            .filter(|line| line.line_type == LineType::Location)
            .map(|line| line.content.to_raw())
            .collect();
        assert_eq!(locations, vec![r"   --> C:\tmp\cs_mstest\Test1.cs:9"]);
    }

    /// Verbatim `dotnet test` output for NUnit (SDK 10.0.401). NUnit repeats
    /// the frame in a numbered `1)  at ...` form, which must not add a second
    /// location to the item.
    const REAL_NUNIT_OUTPUT: &str = r"  Failed FailingTest [26 ms]
  Error Message:
     Assert.That(2 + 2, Is.EqualTo(5))
  Expected: 5
  But was:  4

  Stack Trace:
     at cs_nunit.Tests.FailingTest() in C:\tmp\cs_nunit\UnitTest1.cs:line 8

1)    at cs_nunit.Tests.FailingTest() in C:\tmp\cs_nunit\UnitTest1.cs:line 8

Failed!  - Failed:     1, Passed:     0, Skipped:     0, Total:     1, Duration: 26 ms - cs_nunit.dll (net10.0)";

    #[test]
    fn test_nunit_failure_has_single_location() {
        let report = report_of(REAL_NUNIT_OUTPUT);
        assert_eq!(report.stats.test_fails, 1);
        let locations: Vec<String> = report
            .lines
            .iter()
            .filter(|line| line.line_type == LineType::Location)
            .map(|line| line.content.to_raw())
            .collect();
        assert_eq!(locations, vec![r"   --> C:\tmp\cs_nunit\UnitTest1.cs:8"]);
    }

    /// Both MSTest and NUnit ship Roslyn analyzers whose warnings come through
    /// the normal MSBuild channel during `dotnet test`.
    #[test]
    fn test_analyzer_warnings_during_dotnet_test() {
        let report = report_of(
            r"C:\tmp\cs_nunit\UnitTest1.cs(8,21): warning NUnit2007: The actual value should not be a constant [C:\tmp\cs_nunit\cs_nunit.csproj]",
        );
        assert_eq!(report.stats.warnings, 1);
    }

    /// Known limitation, pinned here so it isn't discovered by surprise: the
    /// .NET CLI localizes its output, and the test keywords are then
    /// untranslatable for the analyzer. Users must set
    /// `DOTNET_CLI_UI_LANGUAGE=en`, which the documentation says.
    ///
    /// This is verbatim French output from SDK 10.0.401.
    #[test]
    fn test_localized_output_is_a_known_limitation() {
        let report = report_of(
            r"  Échoué FailingTest [23 ms]
  Message d'erreur :
     Assert.That(2 + 2, Is.EqualTo(5))
  Arborescence des appels de procédure :
     at cs_nunit.Tests.FailingTest() in C:\tmp\cs_nunit\UnitTest1.cs:line 8

Échoué!  - échec :     1, réussite :     0, ignorée(s) :     0, total :     1, durée : 23 ms - cs_nunit.dll (net10.0)",
        );
        assert_eq!(report.stats.test_fails, 0);
    }
}
