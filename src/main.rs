use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, BufRead, IsTerminal};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use freql::{
    CalculatorSession, Compiler, CompilerOptions, ConfigOverride, InspectStage, RenderOptions,
    ServeOptions, format_source, graph_document, render_dot, render_html, serve,
};
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::{CmdKind, Highlighter};
use rustyline::hint::{Hinter, HistoryHinter};
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{CompletionType, Config, Context, Editor, Helper};

#[derive(Debug, Parser)]
#[command(name = "freql", version, about = "freql static analyzer")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Parse and statically analyze an entry module and its imports.
    Check {
        entry: PathBuf,
        /// Emit schema-v1 JSON, including provisioned pools, stream
        /// cardinalities, schedules, constraints, and obligations.
        #[arg(long)]
        json: bool,
        #[arg(long)]
        require_complete: bool,
        #[arg(long)]
        absolute_paths: bool,
        /// Override an exposed root configuration value (repeatable).
        #[arg(long = "set", value_name = "PATH=EXPR", value_parser = parse_override)]
        set: Vec<ConfigOverride>,
    },
    /// Canonically format freql source files.
    Fmt {
        files: Vec<PathBuf>,
        #[arg(long)]
        check: bool,
    },
    /// Inspect an unstable compiler representation.
    Inspect {
        entry: PathBuf,
        #[arg(long, value_enum)]
        stage: Stage,
        /// Override an exposed root configuration value (repeatable).
        #[arg(long = "set", value_name = "PATH=EXPR", value_parser = parse_override)]
        set: Vec<ConfigOverride>,
    },
    /// Render the elaborated process DAG as DOT or self-contained HTML.
    Render {
        entry: PathBuf,
        /// Emit a self-contained interactive HTML document.
        #[arg(long, conflicts_with = "dot")]
        html: bool,
        /// Emit Graphviz DOT (the default format).
        #[arg(long, conflicts_with = "html")]
        dot: bool,
        /// Render only this process declaration as the graph root.
        #[arg(long)]
        process: Option<String>,
        /// Write output to a file instead of standard output.
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long)]
        absolute_paths: bool,
        /// Override an exposed root configuration value (repeatable).
        #[arg(long = "set", value_name = "PATH=EXPR", value_parser = parse_override)]
        set: Vec<ConfigOverride>,
    },
    /// Serve a browser DAG that recompiles whenever freql sources change.
    Serve {
        entry: PathBuf,
        /// Render only this process declaration as the graph root.
        #[arg(long)]
        process: Option<String>,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// TCP port, or 0 to select a free port.
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Do not open the browser automatically.
        #[arg(long)]
        no_open: bool,
        #[arg(long)]
        absolute_paths: bool,
        /// Override an exposed root configuration value (repeatable).
        #[arg(long = "set", value_name = "PATH=EXPR", value_parser = parse_override)]
        set: Vec<ConfigOverride>,
    },
    /// Run the Language Server Protocol server over standard input/output.
    Lsp,
    /// Start an exact dimensional calculator with persistent imports and lets.
    #[command(alias = "calculator", alias = "repl")]
    Calc {
        /// Directory used to resolve quoted imports.
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Stage {
    Cst,
    Ast,
    Hir,
    Flow,
    Constraints,
    /// Conservative monitored-time projection. This is an unstable inspection
    /// format; use the JSON report for supported integrations.
    Temporal,
}

impl From<Stage> for InspectStage {
    fn from(value: Stage) -> Self {
        match value {
            Stage::Cst => InspectStage::Cst,
            Stage::Ast => InspectStage::Ast,
            Stage::Hir => InspectStage::Hir,
            Stage::Flow => InspectStage::Flow,
            Stage::Constraints => InspectStage::Constraints,
            Stage::Temporal => InspectStage::Temporal,
        }
    }
}

fn parse_override(value: &str) -> Result<ConfigOverride, String> {
    let Some((path, expression)) = value.split_once('=') else {
        return Err("expected PATH=EXPR".into());
    };
    let path = path.trim();
    let expression = expression.trim();
    if path.is_empty() || !path.contains('.') {
        return Err("override path must be process-qualified, for example Factory.rate=10".into());
    }
    if expression.is_empty() {
        return Err("override expression cannot be empty".into());
    }
    Ok(ConfigOverride {
        path: path.into(),
        value: expression.into(),
    })
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("freql: {message}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let cli = Cli::parse();
    match cli.command {
        Command::Check {
            entry,
            json,
            require_complete,
            absolute_paths,
            set,
        } => {
            let compiler = Compiler::new(CompilerOptions {
                absolute_paths,
                overrides: set,
            });
            let compilation = compiler
                .check_path(&entry)
                .map_err(|error| error.to_string())?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&compilation.report)
                        .map_err(|error| error.to_string())?
                );
            } else {
                print!("{}", compilation.render_human());
            }
            Ok(if compilation.report.is_failure(require_complete) {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
        Command::Fmt { files, check } => {
            if files.is_empty() {
                return Err("fmt requires at least one file".into());
            }
            let mut changed = false;
            for file in files {
                let source = fs::read_to_string(&file)
                    .map_err(|error| format!("{}: {error}", file.display()))?;
                let formatted = format_source(&source).map_err(|error| error.to_string())?;
                if formatted != source {
                    changed = true;
                    if !check {
                        fs::write(&file, formatted)
                            .map_err(|error| format!("{}: {error}", file.display()))?;
                    }
                }
            }
            Ok(if check && changed {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
        Command::Inspect { entry, stage, set } => {
            let compiler = Compiler::new(CompilerOptions {
                overrides: set,
                ..CompilerOptions::default()
            });
            let output = compiler
                .inspect_path(&entry, stage.into())
                .map_err(|error| error.to_string())?;
            println!("{output}");
            Ok(ExitCode::SUCCESS)
        }
        Command::Render {
            entry,
            html,
            dot: _,
            process,
            output,
            absolute_paths,
            set,
        } => {
            let compiler = Compiler::new(CompilerOptions {
                absolute_paths,
                overrides: set,
            });
            let compilation = compiler
                .check_path(&entry)
                .map_err(|error| error.to_string())?;
            let document = graph_document(&compilation, &RenderOptions { process })
                .map_err(|error| error.to_string())?;
            let rendered = if html {
                render_html(&document)
            } else {
                render_dot(&document)
            };
            if let Some(path) = output {
                fs::write(&path, rendered)
                    .map_err(|error| format!("{}: {error}", path.display()))?;
            } else {
                print!("{rendered}");
                if !rendered.ends_with('\n') {
                    println!();
                }
            }
            Ok(if compilation.report.is_failure(false) {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
        Command::Serve {
            entry,
            process,
            host,
            port,
            no_open,
            absolute_paths,
            set,
        } => {
            serve(
                &entry,
                CompilerOptions {
                    absolute_paths,
                    overrides: set,
                },
                ServeOptions {
                    host,
                    port,
                    open: !no_open,
                    render: RenderOptions { process },
                },
            )?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Lsp => {
            freql::lsp::run_stdio()?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Calc { root } => run_calculator(root),
    }
}

fn run_calculator(root: PathBuf) -> Result<ExitCode, String> {
    let mut calculator = CalculatorSession::new(root);
    if io::stdin().is_terminal() {
        run_interactive_calculator(&mut calculator)?;
    } else {
        run_piped_calculator(&mut calculator)?;
    }
    Ok(ExitCode::SUCCESS)
}

fn run_interactive_calculator(calculator: &mut CalculatorSession) -> Result<(), String> {
    let config = Config::builder()
        .completion_type(CompletionType::List)
        .completion_show_all_if_ambiguous(true)
        .auto_add_history(true)
        .build();
    let helper = CalculatorHelper::new(
        calculator.base().to_path_buf(),
        calculator.completion_names(),
    );
    let mut editor = Editor::<CalculatorHelper, DefaultHistory>::with_config(config)
        .map_err(|error| error.to_string())?;
    editor.set_helper(Some(helper));
    println!("freql dimensional calculator · ↑/↓ history · Ctrl-R search · Tab complete");
    loop {
        match editor.readline("⁂ ") {
            Ok(line) => {
                let declarations_before = calculator.imports().len()
                    + calculator.units().len()
                    + calculator.bindings().len();
                if !handle_calculator_line(calculator, &line) {
                    break;
                }
                let declarations_after = calculator.imports().len()
                    + calculator.units().len()
                    + calculator.bindings().len();
                if declarations_before != declarations_after || line.trim() == ":clear" {
                    let names = calculator.completion_names();
                    editor
                        .helper_mut()
                        .expect("calculator helper remains installed")
                        .replace_names(names);
                }
            }
            Err(ReadlineError::Interrupted) => {
                println!("^C");
            }
            Err(ReadlineError::Eof) => {
                println!();
                break;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

fn run_piped_calculator(calculator: &mut CalculatorSession) -> Result<(), String> {
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line.map_err(|error| error.to_string())?;
        if !handle_calculator_line(calculator, &line) {
            break;
        }
    }
    Ok(())
}

fn handle_calculator_line(calculator: &mut CalculatorSession, line: &str) -> bool {
    match line.trim() {
        ":quit" | ":q" | "quit" | "exit" => return false,
        ":help" => {
            println!(
                "import \"units.req\" (Unit, ...)\nunit Name = UnitExpression\nlet name: Type = expression\nconst name: Type = expression\nname = expression (also +=, -=, *=, /=, ^=)\nexpression as DisplayUnit\n:bindings  show session declarations\n:clear     reset the session\n:quit      exit\n\n↑/↓ browse history · Ctrl-R search history · Tab complete"
            );
        }
        ":bindings" => {
            if calculator.imports().is_empty()
                && calculator.units().is_empty()
                && calculator.bindings().is_empty()
            {
                println!("(empty)");
            } else {
                for declaration in calculator
                    .imports()
                    .iter()
                    .chain(calculator.units())
                    .chain(calculator.bindings())
                {
                    println!("{declaration}");
                }
            }
        }
        ":clear" => {
            calculator.clear();
            println!("session cleared");
        }
        command if command.starts_with(':') => {
            eprintln!("unknown calculator command {command}; try :help");
        }
        input => {
            match calculator.execute(input) {
                Ok(Some(value)) => println!("{value}"),
                Ok(None) => {}
                Err(error) => eprintln!("{error}"),
            }
            for warning in calculator.warnings() {
                eprintln!("{warning}");
            }
        }
    }
    true
}

struct CalculatorHelper {
    root: PathBuf,
    names: BTreeSet<String>,
    hinter: HistoryHinter,
}

impl CalculatorHelper {
    fn new(root: PathBuf, names: Vec<String>) -> Self {
        let mut helper = Self {
            root,
            names: BTreeSet::new(),
            hinter: HistoryHinter::new(),
        };
        helper.replace_names(names);
        helper
    }

    fn replace_names(&mut self, names: Vec<String>) {
        self.names = names.into_iter().collect();
        self.names.extend(
            [
                "import",
                "let",
                "const",
                "mut",
                "as",
                "true",
                "false",
                "std.process",
                "std.collection",
                "std.stream",
                ":help",
                ":bindings",
                ":clear",
                ":quit",
            ]
            .into_iter()
            .map(str::to_owned),
        );
    }

    fn import_path_candidates(&self, line: &str, pos: usize) -> Option<(usize, Vec<Pair>)> {
        let prefix = &line[..pos];
        let quote = prefix.rfind('"')?;
        if !prefix[..quote].trim_start().starts_with("import") {
            return None;
        }
        let typed = &prefix[quote + 1..];
        let typed_path = std::path::Path::new(typed);
        let parent = typed_path.parent().unwrap_or(std::path::Path::new(""));
        let leaf = typed_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let directory = self.root.join(parent);
        let mut candidates = fs::read_dir(directory)
            .ok()?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !name.starts_with(leaf) {
                    return None;
                }
                let is_directory = entry.file_type().ok()?.is_dir();
                if !is_directory
                    && entry.path().extension().and_then(|ext| ext.to_str()) != Some("req")
                {
                    return None;
                }
                let replacement = if parent.as_os_str().is_empty() {
                    name
                } else {
                    format!("{}/{}", parent.to_string_lossy(), name)
                };
                let replacement = if is_directory {
                    format!("{replacement}/")
                } else {
                    replacement
                };
                Some(Pair {
                    display: replacement.clone(),
                    replacement,
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.replacement.cmp(&right.replacement));
        Some((quote + 1, candidates))
    }
}

impl Completer for CalculatorHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _context: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        if let Some(candidates) = self.import_path_candidates(line, pos) {
            return Ok(candidates);
        }
        let start = line[..pos]
            .char_indices()
            .rev()
            .find(|(_, character)| {
                !character.is_ascii_alphanumeric()
                    && *character != '_'
                    && *character != '.'
                    && *character != ':'
            })
            .map_or(0, |(index, character)| index + character.len_utf8());
        let partial = &line[start..pos];
        let candidates = self
            .names
            .iter()
            .filter(|name| name.starts_with(partial) && name.as_str() != partial)
            .map(|name| Pair {
                display: name.clone(),
                replacement: name.clone(),
            })
            .collect();
        Ok((start, candidates))
    }
}

impl Hinter for CalculatorHelper {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, context: &Context<'_>) -> Option<String> {
        self.hinter.hint(line, pos, context)
    }
}

impl Highlighter for CalculatorHelper {
    fn highlight<'line>(&self, line: &'line str, _pos: usize) -> Cow<'line, str> {
        let lexed = freql::lexer::lex(line);
        let mut output = String::with_capacity(line.len() + 32);
        let mut cursor = 0;
        for token in lexed.tokens {
            output.push_str(&line[cursor..token.span.start]);
            let color = match token.kind {
                freql::syntax::SyntaxKind::Comment => Some("\x1b[2;90m"),
                freql::syntax::SyntaxKind::String => Some("\x1b[32m"),
                freql::syntax::SyntaxKind::Number => Some("\x1b[33m"),
                freql::syntax::SyntaxKind::Ident
                    if matches!(
                        token.text.as_str(),
                        "import"
                            | "let"
                            | "const"
                            | "mut"
                            | "as"
                            | "true"
                            | "false"
                            | "and"
                            | "or"
                            | "not"
                    ) =>
                {
                    Some("\x1b[1;35m")
                }
                freql::syntax::SyntaxKind::Ident
                    if self.names.contains(&token.text)
                        || token.text.starts_with(char::is_uppercase) =>
                {
                    Some("\x1b[36m")
                }
                freql::syntax::SyntaxKind::Plus
                | freql::syntax::SyntaxKind::Minus
                | freql::syntax::SyntaxKind::Star
                | freql::syntax::SyntaxKind::Slash
                | freql::syntax::SyntaxKind::Caret
                | freql::syntax::SyntaxKind::Eq => Some("\x1b[34m"),
                _ => None,
            };
            if let Some(color) = color {
                output.push_str(color);
                output.push_str(&token.text);
                output.push_str("\x1b[0m");
            } else {
                output.push_str(&token.text);
            }
            cursor = token.span.end;
        }
        output.push_str(&line[cursor..]);
        Cow::Owned(output)
    }

    fn highlight_prompt<'borrow, 'self_ref: 'borrow, 'prompt: 'borrow>(
        &'self_ref self,
        prompt: &'prompt str,
        _default: bool,
    ) -> Cow<'borrow, str> {
        Cow::Owned(format!("\x1b[1;36m{prompt}\x1b[0m"))
    }

    fn highlight_hint<'hint>(&self, hint: &'hint str) -> Cow<'hint, str> {
        Cow::Owned(format!("\x1b[2;90m{hint}\x1b[0m"))
    }

    fn highlight_candidate<'candidate>(
        &self,
        candidate: &'candidate str,
        _completion: CompletionType,
    ) -> Cow<'candidate, str> {
        Cow::Owned(format!("\x1b[36m{candidate}\x1b[0m"))
    }

    fn highlight_char(&self, _line: &str, _pos: usize, _kind: CmdKind) -> bool {
        true
    }
}

impl Validator for CalculatorHelper {}
impl Helper for CalculatorHelper {}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use clap::Parser;
    use rustyline::Context;
    use rustyline::completion::Completer;
    use rustyline::highlight::Highlighter;
    use rustyline::history::DefaultHistory;
    use tempfile::tempdir;

    use super::{CalculatorHelper, Cli, Command, Stage};

    #[test]
    fn render_defaults_to_dot_and_accepts_process_selection() {
        let cli =
            Cli::try_parse_from(["freql", "render", "main.req", "--process", "Factory"]).unwrap();
        let Command::Render {
            html, dot, process, ..
        } = cli.command
        else {
            panic!("expected render command");
        };
        assert!(!html);
        assert!(!dot);
        assert_eq!(process.as_deref(), Some("Factory"));
    }

    #[test]
    fn render_formats_are_mutually_exclusive() {
        let error =
            Cli::try_parse_from(["freql", "render", "main.req", "--html", "--dot"]).unwrap_err();
        assert!(error.to_string().contains("cannot be used with"));
    }

    #[test]
    fn serve_defaults_to_ephemeral_loopback_and_opens_browser() {
        let cli = Cli::try_parse_from(["freql", "serve", "main.req"]).unwrap();
        let Command::Serve {
            host,
            port,
            no_open,
            ..
        } = cli.command
        else {
            panic!("expected serve command");
        };
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 0);
        assert!(!no_open);
    }

    #[test]
    fn set_is_repeatable_and_requires_a_qualified_path() {
        let cli = Cli::try_parse_from([
            "freql",
            "check",
            "main.req",
            "--set",
            "Factory.rate=7 Gigabit / Second",
            "--set",
            "Factory.lanes=10",
        ])
        .unwrap();
        let Command::Check { set, .. } = cli.command else {
            panic!("expected check command");
        };
        assert_eq!(set.len(), 2);
        assert_eq!(set[0].path, "Factory.rate");
        assert_eq!(set[0].value, "7 Gigabit / Second");

        let error =
            Cli::try_parse_from(["freql", "check", "main.req", "--set", "rate=20"]).unwrap_err();
        assert!(error.to_string().contains("process-qualified"));
    }

    #[test]
    fn inspect_accepts_the_temporal_projection() {
        let cli =
            Cli::try_parse_from(["freql", "inspect", "main.req", "--stage", "temporal"]).unwrap();
        let Command::Inspect { stage, .. } = cli.command else {
            panic!("expected inspect command");
        };
        assert!(matches!(stage, Stage::Temporal));
    }

    #[test]
    fn calculator_has_friendly_aliases_and_a_relative_import_root() {
        for command in ["calc", "calculator", "repl"] {
            let cli = Cli::try_parse_from(["freql", command, "--root", "model"]).unwrap();
            let Command::Calc { root } = cli.command else {
                panic!("expected calculator command");
            };
            assert_eq!(root, PathBuf::from("model"));
        }
    }

    #[test]
    fn calculator_helper_completes_names_and_import_paths_and_highlights_syntax() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("units.req"),
            "unit Minute = Second * 60\n",
        )
        .unwrap();
        let helper = CalculatorHelper::new(
            directory.path().to_path_buf(),
            vec!["Minute".into(), "rate".into()],
        );
        let history = DefaultHistory::new();
        let context = Context::new(&history);

        let (_, names) = helper.complete("rate as Min", 11, &context).unwrap();
        assert!(
            names
                .iter()
                .any(|candidate| candidate.replacement == "Minute")
        );

        let (start, paths) = helper.complete("import \"uni", 11, &context).unwrap();
        assert_eq!(start, 8);
        assert!(
            paths
                .iter()
                .any(|candidate| candidate.replacement == "units.req")
        );

        let highlighted = helper.highlight("let rate = 2 Minute # exact", 27);
        assert!(highlighted.contains("\x1b[1;35mlet\x1b[0m"));
        assert!(highlighted.contains("\x1b[36mMinute\x1b[0m"));
        assert!(highlighted.contains("\x1b[2;90m# exact\x1b[0m"));
    }
}
