//! A feature-showcase REPL built on rustline.
//!
//! Run with `cargo run --example repl`. History is persisted to
//! `~/.rustline_history`.

use rustline::{
    CommandCompleter, CompletionProvider, Completions, Config, FileCompleter, Hint, HintProvider,
    Rustline, RustlineError, history_path, word_start,
};
use std::borrow::Cow;
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

// ---------------------------------------------------------------- errors

#[derive(Debug)]
enum ReplError {
    Io(std::io::Error),
    Parse(String),
    Command(String),
    VariableNotFound(String),
}

impl fmt::Display for ReplError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReplError::Io(e) => write!(f, "i/o error: {e}"),
            ReplError::Parse(msg) => write!(f, "parse error: {msg}"),
            ReplError::Command(msg) => write!(f, "command error: {msg}"),
            ReplError::VariableNotFound(name) => write!(f, "variable '{name}' not found"),
        }
    }
}

impl From<std::io::Error> for ReplError {
    fn from(err: std::io::Error) -> Self {
        ReplError::Io(err)
    }
}

type ReplResult<T> = Result<T, ReplError>;

/// What the loop should do after a command.
#[derive(PartialEq, Eq)]
enum Outcome {
    Continue,
    Quit,
}

// ------------------------------------------------------------ completion

const COMMANDS: &[&str] = &[
    "alias",
    "bench",
    "calc",
    "cat",
    "cd",
    "clear",
    "config",
    "debug",
    "echo",
    "exit",
    "get",
    "help",
    "history",
    "ls",
    "password",
    "pwd",
    "quit",
    "set",
    "unalias",
    "unicode-test",
];

/// Completes commands first, then environment variables, then paths.
struct SmartCompleter {
    commands: CommandCompleter<FileCompleter>,
}

impl SmartCompleter {
    fn new() -> Self {
        SmartCompleter {
            commands: CommandCompleter::new(COMMANDS.iter().copied(), FileCompleter::new()),
        }
    }
}

impl CompletionProvider for SmartCompleter {
    fn complete(&self, line: &str, pos: usize) -> Completions {
        let start = word_start(line, pos);
        let word = &line[start..pos];

        // `$FOO` completes against the environment wherever it appears.
        if let Some(prefix) = word.strip_prefix('$') {
            let mut completions = Completions::new(start);
            let prefix = prefix.to_uppercase();
            for (key, _) in env::vars() {
                if key.to_uppercase().starts_with(&prefix) {
                    completions.add(format!("${key}"));
                }
            }
            completions.sort_dedup();
            return completions;
        }

        self.commands.complete(line, pos)
    }
}

/// Suggests the rest of a command name as dim text after the cursor.
struct CommandHinter;

impl HintProvider for CommandHinter {
    fn hint(&self, line: &str, pos: usize) -> Option<Hint> {
        if line.is_empty() || pos != line.len() || line.contains(' ') {
            return None;
        }
        let candidate = COMMANDS
            .iter()
            .find(|c| c.starts_with(line) && **c != line)?;
        Some(Hint::new(&candidate[line.len()..]))
    }
}

// ----------------------------------------------------------------- state

struct ReplState {
    variables: HashMap<String, String>,
    aliases: HashMap<String, String>,
    current_dir: PathBuf,
    command_count: usize,
    debug_mode: bool,
}

impl ReplState {
    fn new() -> Self {
        ReplState {
            variables: HashMap::new(),
            aliases: HashMap::new(),
            current_dir: env::current_dir().unwrap_or_default(),
            command_count: 0,
            debug_mode: false,
        }
    }

    /// Substitutes `$name` from the local variables, then the environment.
    fn expand_variables<'a>(&self, input: &'a str) -> Cow<'a, str> {
        if !input.contains('$') {
            return Cow::Borrowed(input);
        }
        let mut result = input.to_string();
        for (name, value) in &self.variables {
            result = result.replace(&format!("${name}"), value);
        }
        for (key, value) in env::vars() {
            let pattern = format!("${key}");
            if result.contains(&pattern) {
                result = result.replace(&pattern, &value);
            }
        }
        Cow::Owned(result)
    }

    /// Replaces a leading alias with its expansion.
    fn expand_aliases<'a>(&self, command: &'a str) -> Cow<'a, str> {
        let end = command.find(char::is_whitespace).unwrap_or(command.len());
        match self.aliases.get(&command[..end]) {
            Some(alias) if end < command.len() => Cow::Owned(format!("{alias}{}", &command[end..])),
            Some(alias) => Cow::Owned(alias.clone()),
            None => Cow::Borrowed(command),
        }
    }
}

// -------------------------------------------------------------- commands

fn execute(command: &str, state: &mut ReplState, rl: &mut Rustline) -> ReplResult<Outcome> {
    let expanded = state.expand_aliases(command);
    let trimmed = expanded.trim();
    if trimmed.is_empty() {
        return Ok(Outcome::Continue);
    }

    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    state.command_count += 1;

    match parts[0] {
        "exit" | "quit" => return Ok(Outcome::Quit),
        "help" => print_help(),
        "debug" => {
            state.debug_mode = !state.debug_mode;
            println!("debug mode: {}", on_off(state.debug_mode));
        }
        "bench" => run_benchmarks(state),
        "history" => show_history(rl),
        "clear" => rustline::clear_screen().map_err(|e| ReplError::Command(e.to_string()))?,
        "set" => return set_variable(&parts, state),
        "get" => return get_variable(&parts, state),
        "echo" => println!("{}", state.expand_variables(&parts[1..].join(" "))),
        "pwd" => println!("{}", state.current_dir.display()),
        "cd" => return change_dir(&parts, state),
        "ls" => return list_dir(&parts, state),
        "cat" => return show_file(&parts, state),
        "alias" => return manage_alias(&parts, state),
        "unalias" => return remove_alias(&parts, state),
        "password" => return read_password(rl),
        "unicode-test" => unicode_test(),
        "config" => show_config(state, rl),
        "calc" => return calculate(&parts),
        other => return Err(ReplError::Command(format!("unknown command: {other}"))),
    }
    Ok(Outcome::Continue)
}

fn on_off(flag: bool) -> &'static str {
    if flag { "on" } else { "off" }
}

fn set_variable(parts: &[&str], state: &mut ReplState) -> ReplResult<Outcome> {
    if parts.len() < 3 {
        return Err(ReplError::Command("usage: set <variable> <value>".into()));
    }
    let value = parts[2..].join(" ");
    println!("${} = {value:?}", parts[1]);
    state.variables.insert(parts[1].to_string(), value);
    Ok(Outcome::Continue)
}

fn get_variable(parts: &[&str], state: &ReplState) -> ReplResult<Outcome> {
    if parts.len() < 2 {
        println!("variables:");
        for (name, value) in &state.variables {
            println!("  ${name} = {value:?}");
        }
        return Ok(Outcome::Continue);
    }
    let name = parts[1];
    match state.variables.get(name) {
        Some(value) => println!("${name} = {value:?}"),
        None => match env::var(name) {
            Ok(value) => println!("${name} = {value:?} (env)"),
            Err(_) => return Err(ReplError::VariableNotFound(name.to_string())),
        },
    }
    Ok(Outcome::Continue)
}

fn change_dir(parts: &[&str], state: &mut ReplState) -> ReplResult<Outcome> {
    let path = if parts.len() >= 2 {
        state.expand_variables(parts[1])
    } else {
        env::var("HOME").map_or(Cow::Borrowed("."), Cow::Owned)
    };
    env::set_current_dir(path.as_ref())?;
    state.current_dir = env::current_dir()?;
    println!("{}", state.current_dir.display());
    Ok(Outcome::Continue)
}

fn list_dir(parts: &[&str], state: &ReplState) -> ReplResult<Outcome> {
    let dir = if parts.len() >= 2 {
        state.expand_variables(parts[1])
    } else {
        Cow::Borrowed(".")
    };

    let mut entries: Vec<_> = fs::read_dir(dir.as_ref())?.flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);

    for entry in entries {
        let metadata = entry.metadata();
        let is_dir = metadata.as_ref().is_ok_and(|m| m.is_dir());
        let is_exec = metadata
            .as_ref()
            .is_ok_and(|m| m.permissions().mode() & 0o111 != 0);
        let color = if is_dir {
            "\x1b[34m"
        } else if is_exec {
            "\x1b[32m"
        } else {
            ""
        };
        println!("  {color}{}\x1b[0m", entry.file_name().to_string_lossy());
    }
    Ok(Outcome::Continue)
}

fn show_file(parts: &[&str], state: &ReplState) -> ReplResult<Outcome> {
    if parts.len() < 2 {
        return Err(ReplError::Command("usage: cat <filename>".into()));
    }
    let filename = state.expand_variables(parts[1]);
    print!("{}", fs::read_to_string(filename.as_ref())?);
    Ok(Outcome::Continue)
}

fn manage_alias(parts: &[&str], state: &mut ReplState) -> ReplResult<Outcome> {
    match parts.len() {
        0 | 1 => {
            if state.aliases.is_empty() {
                println!("no aliases defined");
            }
            for (name, value) in &state.aliases {
                println!("  {name} = {value:?}");
            }
        }
        2 => match state.aliases.get(parts[1]) {
            Some(value) => println!("{} = {value:?}", parts[1]),
            None => {
                return Err(ReplError::Command(format!(
                    "alias '{}' not found",
                    parts[1]
                )));
            }
        },
        _ => {
            let value = parts[2..].join(" ");
            println!("{} = {value:?}", parts[1]);
            state.aliases.insert(parts[1].to_string(), value);
        }
    }
    Ok(Outcome::Continue)
}

fn remove_alias(parts: &[&str], state: &mut ReplState) -> ReplResult<Outcome> {
    if parts.len() < 2 {
        return Err(ReplError::Command("usage: unalias <name>".into()));
    }
    if state.aliases.remove(parts[1]).is_none() {
        return Err(ReplError::Command(format!(
            "alias '{}' not found",
            parts[1]
        )));
    }
    println!("removed alias '{}'", parts[1]);
    Ok(Outcome::Continue)
}

/// Demonstrates mask mode: the typed text is drawn as asterisks and is never
/// added to history.
fn read_password(rl: &mut Rustline) -> ReplResult<Outcome> {
    match rl.read_password("password: ") {
        Ok(password) => println!("read {} characters", password.chars().count()),
        Err(RustlineError::Interrupted | RustlineError::Eof) => println!("cancelled"),
        Err(e) => return Err(ReplError::Command(e.to_string())),
    }
    Ok(Outcome::Continue)
}

fn calculate(parts: &[&str]) -> ReplResult<Outcome> {
    if parts.len() < 2 {
        return Err(ReplError::Command("usage: calc <expression>".into()));
    }
    let expr = parts[1..].join(" ");
    match evaluate(&expr) {
        Ok(result) => println!("{expr} = {result}"),
        Err(e) => return Err(ReplError::Parse(e)),
    }
    Ok(Outcome::Continue)
}

fn evaluate(expr: &str) -> Result<f64, String> {
    let tokens: Vec<&str> = expr.split_whitespace().collect();
    if tokens.len() != 3 {
        return Err("expected: number operator number".to_string());
    }
    let left: f64 = tokens[0]
        .parse()
        .map_err(|_| format!("invalid number: {}", tokens[0]))?;
    let right: f64 = tokens[2]
        .parse()
        .map_err(|_| format!("invalid number: {}", tokens[2]))?;

    match tokens[1] {
        "+" => Ok(left + right),
        "-" => Ok(left - right),
        "*" => Ok(left * right),
        "/" if right == 0.0 => Err("division by zero".to_string()),
        "/" => Ok(left / right),
        "%" => Ok(left % right),
        "^" | "**" => Ok(left.powf(right)),
        op => Err(format!("unknown operator: {op}")),
    }
}

// --------------------------------------------------------------- display

fn show_history(rl: &Rustline) {
    let history = rl.history();
    if history.is_empty() {
        println!("(no history yet)");
        return;
    }
    let skip = history.len().saturating_sub(20);
    for (i, entry) in history.iter().enumerate().skip(skip) {
        println!("  {:3}: {entry}", i + 1);
    }
    if skip > 0 {
        println!("  ... and {skip} older entries");
    }
}

fn unicode_test() {
    println!("  emoji:       😀 🎉 🚀 🌟");
    println!("  chinese:     你好世界");
    println!("  japanese:    こんにちは世界");
    println!("  korean:      안녕하세요");
    println!("  arabic:      مرحبا بالعالم");
    println!("  russian:     Привет мир");
    println!("  greek:       Γεια σου κόσμε");
    println!("  math:        ∫ ∑ ∏ √ ∞ ≈ ≠ ≤ ≥ ∀ ∃ ∈ ∉");
    println!("  box drawing: ┌─┬─┐ │ │ │ ├─┼─┤ └─┴─┘");
}

fn show_config(state: &ReplState, rl: &Rustline) {
    let config = rl.config();
    println!("configuration:");
    println!("  history max size:   {}", config.history_max_size);
    println!("  hints:              {}", on_off(config.enable_hints));
    println!("  completion:         {}", on_off(config.enable_completion));
    println!("  multiline:          {}", on_off(config.enable_multiline));
    println!(
        "  bracketed paste:    {}",
        on_off(config.enable_bracketed_paste)
    );
    println!("  balance pairs:      {}", on_off(config.balance_pairs));
    println!(
        "  highlight brackets: {}",
        on_off(config.highlight_brackets)
    );
    println!("state:");
    println!("  commands executed:  {}", state.command_count);
    println!("  variables defined:  {}", state.variables.len());
    println!("  aliases defined:    {}", state.aliases.len());
    println!("  history entries:    {}", rl.history().len());
    println!("  debug mode:         {}", on_off(state.debug_mode));
}

fn run_benchmarks(state: &ReplState) {
    use std::time::Instant;

    let start = Instant::now();
    for _ in 0..10_000 {
        let _ = state.expand_variables("Hello $USER from $HOME");
    }
    println!("  variable expansion (10k): {:?}", start.elapsed());

    let start = Instant::now();
    for _ in 0..10_000 {
        let _ = state.expand_aliases("ls -la");
    }
    println!("  alias expansion (10k):    {:?}", start.elapsed());

    let start = Instant::now();
    for _ in 0..10_000 {
        let _ = evaluate("42 + 42");
    }
    println!("  expression eval (10k):    {:?}", start.elapsed());
}

fn print_help() {
    println!(
        "\
commands:
  help              show this message
  exit, quit        leave the repl
  history           show recent history
  clear             clear the screen
  set NAME VALUE    define a variable
  get [NAME]        show a variable, or all of them
  echo TEXT         echo text with $variable expansion
  pwd               print the working directory
  cd [DIR]          change directory
  ls [DIR]          list a directory
  cat FILE          print a file
  alias N CMD       define an alias
  unalias N         remove an alias
  calc EXPR         evaluate 'number operator number'
  password          read a masked line (never stored in history)
  unicode-test      print a unicode sample
  config            show configuration and state
  debug             toggle debug output
  bench             run micro benchmarks

editing:
  CTRL-A/E          start / end of line       CTRL-T   transpose characters
  CTRL-B/F          back / forward a char     ALT-T    transpose words
  ALT-B/F           back / forward a word     ALT-U/L  upper / lowercase word
  ALT-LEFT/RIGHT    back / forward an expr    ALT-C    capitalize word
  CTRL-P/N          previous / next history   ALT-\\    squeeze whitespace
  CTRL-R            search history            CTRL-Y   yank
  CTRL-G            cancel search             ALT-Y    rotate ring and yank
  CTRL-K/U          kill to end / start       CTRL-L   clear screen
  CTRL-W, ALT-H     kill word backwards       CTRL-Z   suspend
  ALT-D             kill word forwards        CTRL-C   interrupt
  CTRL-SPACE        set mark                  CTRL-D   delete, or exit
  CTRL-X CTRL-X     go to mark                TAB      complete
  ALT-SHIFT-B/S     barf / slurp expression   CTRL-Q   escaped insert

  Type '(' without closing it to see multiline continuation."
    );
}

// ------------------------------------------------------------------ main

fn main() {
    let config = Config {
        // Keep reading until parentheses balance, to demonstrate multiline.
        balance_pairs: true,
        ..Config::default()
    };

    let mut rl = Rustline::with_config(config);
    rl.set_completer(Box::new(SmartCompleter::new()));
    rl.set_hinter(Box::new(CommandHinter));

    let history_file = history_path("rustline");
    if let Err(e) = rl.load_history(&history_file) {
        eprintln!("could not load history: {e}");
    }

    println!("rustline repl -- type 'help' for commands, 'exit' to quit");

    let mut state = ReplState::new();

    loop {
        let prompt = format!("\x1b[32mrustline\x1b[0m:{}> ", state.command_count + 1);
        match rl.readline(&prompt) {
            Ok(line) => {
                if line.trim().is_empty() {
                    continue;
                }
                rl.add_history_entry(&line);
                match execute(&line, &mut state, &mut rl) {
                    Ok(Outcome::Quit) => break,
                    Ok(Outcome::Continue) => {}
                    Err(e) => {
                        eprintln!("error: {e}");
                        if state.debug_mode {
                            eprintln!("  debug: {e:?}");
                        }
                    }
                }
            }
            Err(RustlineError::Interrupted) => {
                println!("interrupted -- type 'exit' to quit");
            }
            Err(RustlineError::Eof) => {
                println!("goodbye");
                break;
            }
            Err(e) => {
                eprintln!("readline error: {e}");
                break;
            }
        }
    }

    if let Err(e) = rl.save_history(&history_file) {
        eprintln!("could not save history: {e}");
    }
    println!("commands executed: {}", state.command_count);
}
