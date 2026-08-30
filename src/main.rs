use rustline::{CompletionProvider, Completions, Config, FileCompleter, Rustline};
use std::borrow::Cow;
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

type ReplResult<T> = Result<T, ReplError>;

#[derive(Debug)]
enum ReplError {
    Io(std::io::Error),
    ParseError(String),
    CommandError(String),
    VariableNotFound(String),
}

impl fmt::Display for ReplError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReplError::Io(e) => write!(f, "I/O error: {}", e),
            ReplError::ParseError(msg) => write!(f, "Parse error: {}", msg),
            ReplError::CommandError(msg) => write!(f, "Command error: {}", msg),
            ReplError::VariableNotFound(name) => write!(f, "Variable '{}' not found", name),
        }
    }
}

impl From<std::io::Error> for ReplError {
    #[inline]
    fn from(err: std::io::Error) -> Self {
        ReplError::Io(err)
    }
}

struct SmartCompleter {
    file_completer: FileCompleter,
    commands: Vec<&'static str>,
}

impl SmartCompleter {
    const BUILTIN_COMMANDS: &'static [&'static str] = &[
        "help",
        "exit",
        "quit",
        "history",
        "clear",
        "set",
        "get",
        "echo",
        "cd",
        "pwd",
        "ls",
        "cat",
        "save",
        "load",
        "eval",
        "multiline",
        "unicode-test",
        "config",
        "calc",
        "alias",
        "unalias",
        "bench",
        "debug",
    ];

    #[inline]
    fn new() -> Self {
        SmartCompleter {
            file_completer: FileCompleter::new(),
            commands: Self::BUILTIN_COMMANDS.to_vec(),
        }
    }
}

impl CompletionProvider for SmartCompleter {
    fn complete(&self, line: &str, pos: usize) -> Completions {
        let mut completions = Completions::new();

        let word_start = line[..pos]
            .rfind(char::is_whitespace)
            .map(|i| i + 1)
            .unwrap_or(0);

        let word = &line[word_start..pos];
        let line_start = &line[..word_start];

        if line_start.trim().is_empty() {
            for &cmd in &self.commands {
                if cmd.starts_with(word) {
                    completions.add(cmd.to_string());
                }
            }
        } else if let Some(var_prefix) = word.strip_prefix('$') {
            for (key, _) in env::vars() {
                if key.to_lowercase().starts_with(&var_prefix.to_lowercase()) {
                    completions.add(format!("${}", key));
                }
            }
        } else {
            return self.file_completer.complete(line, pos);
        }

        completions
    }
}

struct ReplState {
    variables: HashMap<String, String>,
    aliases: HashMap<String, String>,
    current_dir: PathBuf,
    command_count: usize,
    multiline_mode: bool,
    history: Vec<String>,
    debug_mode: bool,
}

impl ReplState {
    #[inline]
    fn new() -> Self {
        ReplState {
            variables: HashMap::with_capacity(16),
            aliases: HashMap::with_capacity(8),
            current_dir: env::current_dir().unwrap_or_default(),
            command_count: 0,
            multiline_mode: false,
            history: Vec::with_capacity(1024),
            debug_mode: false,
        }
    }

    #[inline]
    fn expand_variables<'a>(&self, input: &'a str) -> Cow<'a, str> {
        if !input.contains('$') {
            return Cow::Borrowed(input);
        }

        let mut result = input.to_string();

        for (name, value) in &self.variables {
            let pattern = format!("${}", name);
            if result.contains(&pattern) {
                result = result.replace(&pattern, value);
            }
        }

        for (key, value) in env::vars() {
            let pattern = format!("${}", key);
            if result.contains(&pattern) {
                result = result.replace(&pattern, &value);
            }
        }

        Cow::Owned(result)
    }

    #[inline]
    fn expand_aliases<'a>(&self, command: &'a str) -> Cow<'a, str> {
        let first_word_end = command.find(char::is_whitespace).unwrap_or(command.len());
        let first_word = &command[..first_word_end];

        if let Some(alias) = self.aliases.get(first_word) {
            if first_word_end < command.len() {
                Cow::Owned(format!("{}{}", alias, &command[first_word_end..]))
            } else {
                Cow::Owned(alias.clone())
            }
        } else {
            Cow::Borrowed(command)
        }
    }
}

#[inline]
fn execute_command(command: &str, state: &mut ReplState) -> ReplResult<bool> {
    let expanded_command = state.expand_aliases(command);
    let trimmed = expanded_command.trim();

    if trimmed.is_empty() {
        return Ok(true);
    }

    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    if parts.is_empty() {
        return Ok(true);
    }

    state.command_count += 1;

    match parts[0] {
        "exit" | "quit" => Ok(false),

        "help" => {
            print_help();
            Ok(true)
        }

        "debug" => {
            state.debug_mode = !state.debug_mode;
            println!(
                "🐛 Debug mode: {}",
                if state.debug_mode { "ON" } else { "OFF" }
            );
            Ok(true)
        }

        "bench" => {
            run_micro_benchmarks(state);
            Ok(true)
        }

        "history" => {
            display_history(state);
            Ok(true)
        }

        "clear" => {
            print!("\x1b[H\x1b[2J");
            Ok(true)
        }

        "set" => handle_set_command(&parts, state),

        "get" => handle_get_command(&parts, state),

        "echo" => {
            let text = parts[1..].join(" ");
            let expanded = state.expand_variables(&text);
            println!("{}", expanded);
            Ok(true)
        }

        "pwd" => {
            println!("📁 {}", state.current_dir.display());
            Ok(true)
        }

        "cd" => handle_cd_command(&parts, state),

        "ls" => handle_ls_command(&parts, state),

        "cat" => handle_cat_command(&parts, state),

        "alias" => handle_alias_command(&parts, state),

        "unalias" => handle_unalias_command(&parts, state),

        "multiline" => {
            state.multiline_mode = !state.multiline_mode;
            let status = if state.multiline_mode { "ON" } else { "OFF" };
            println!("📝 Multiline mode: {}", status);
            if state.multiline_mode {
                println!("   Use empty line or Ctrl+D to execute");
            }
            Ok(true)
        }

        "unicode-test" => {
            display_unicode_test();
            Ok(true)
        }

        "config" => {
            display_config(state);
            Ok(true)
        }

        "calc" => handle_calc_command(&parts),

        "eval" => handle_eval_command(&parts, state),

        _ => Err(ReplError::CommandError(format!(
            "Unknown command: {}",
            parts[0]
        ))),
    }
}

#[inline]
fn handle_set_command(parts: &[&str], state: &mut ReplState) -> ReplResult<bool> {
    if parts.len() >= 3 {
        let var_name = parts[1].to_string();
        let var_value = parts[2..].join(" ");
        state.variables.insert(var_name.clone(), var_value.clone());
        println!("✅ Set ${} = \"{}\"", var_name, var_value);
    } else {
        return Err(ReplError::CommandError(
            "Usage: set <variable> <value>".to_string(),
        ));
    }
    Ok(true)
}

#[inline]
fn handle_get_command(parts: &[&str], state: &mut ReplState) -> ReplResult<bool> {
    if parts.len() >= 2 {
        let var_name = parts[1];
        match state.variables.get(var_name) {
            Some(value) => println!("${} = \"{}\"", var_name, value),
            None => match env::var(var_name) {
                Ok(value) => println!("${} = \"{}\" (env)", var_name, value),
                Err(_) => return Err(ReplError::VariableNotFound(var_name.to_string())),
            },
        }
    } else {
        display_all_variables(state);
    }
    Ok(true)
}

#[inline]
fn handle_cd_command(parts: &[&str], state: &mut ReplState) -> ReplResult<bool> {
    let path = if parts.len() >= 2 {
        state.expand_variables(parts[1])
    } else {
        env::var("HOME")
            .map(Cow::Owned)
            .unwrap_or(Cow::Borrowed("."))
    };

    env::set_current_dir(path.as_ref())?;
    state.current_dir = env::current_dir()?;
    println!("📁 Changed to: {}", state.current_dir.display());
    Ok(true)
}

#[inline]
fn handle_ls_command(parts: &[&str], state: &mut ReplState) -> ReplResult<bool> {
    let dir = if parts.len() >= 2 {
        state.expand_variables(parts[1])
    } else {
        Cow::Borrowed(".")
    };

    let entries = fs::read_dir(dir.as_ref())?;
    println!("📂 Contents of {}:", dir);

    let mut items: Vec<_> = entries.flatten().collect();
    items.sort_by_key(|e| e.file_name());

    for entry in items {
        let name = entry.file_name();
        let metadata = entry.metadata();
        let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let is_exec = metadata
            .as_ref()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);

        let (prefix, color) = if is_dir {
            ("📁", "\x1b[34m")
        } else if is_exec {
            ("🔧", "\x1b[32m")
        } else {
            ("📄", "\x1b[0m")
        };

        println!("  {} {}{}\x1b[0m", prefix, color, name.to_string_lossy());
    }
    Ok(true)
}

#[inline]
fn handle_cat_command(parts: &[&str], state: &mut ReplState) -> ReplResult<bool> {
    if parts.len() >= 2 {
        let filename = state.expand_variables(parts[1]);
        let content = fs::read_to_string(filename.as_ref())?;
        println!("📄 Contents of {}:", filename);
        println!("{}", content);
    } else {
        return Err(ReplError::CommandError("Usage: cat <filename>".to_string()));
    }
    Ok(true)
}

#[inline]
fn handle_alias_command(parts: &[&str], state: &mut ReplState) -> ReplResult<bool> {
    if parts.len() >= 3 {
        let alias_name = parts[1].to_string();
        let alias_value = parts[2..].join(" ");
        state
            .aliases
            .insert(alias_name.clone(), alias_value.clone());
        println!("✅ Alias created: {} = '{}'", alias_name, alias_value);
    } else if parts.len() == 2 {
        match state.aliases.get(parts[1]) {
            Some(value) => println!("{} = '{}'", parts[1], value),
            None => {
                return Err(ReplError::CommandError(format!(
                    "Alias '{}' not found",
                    parts[1]
                )));
            }
        }
    } else if state.aliases.is_empty() {
        println!("📋 No aliases defined");
    } else {
        println!("📋 Aliases:");
        for (name, value) in &state.aliases {
            println!("  {} = '{}'", name, value);
        }
    }
    Ok(true)
}

#[inline]
fn handle_unalias_command(parts: &[&str], state: &mut ReplState) -> ReplResult<bool> {
    if parts.len() >= 2 {
        if state.aliases.remove(parts[1]).is_some() {
            println!("✅ Removed alias '{}'", parts[1]);
        } else {
            return Err(ReplError::CommandError(format!(
                "Alias '{}' not found",
                parts[1]
            )));
        }
    } else {
        return Err(ReplError::CommandError("Usage: unalias <name>".to_string()));
    }
    Ok(true)
}

#[inline]
fn handle_calc_command(parts: &[&str]) -> ReplResult<bool> {
    if parts.len() >= 2 {
        let expr = parts[1..].join(" ");
        match evaluate_expression(&expr) {
            Ok(result) => println!("🧮 {} = {}", expr, result),
            Err(e) => return Err(ReplError::ParseError(e)),
        }
    } else {
        return Err(ReplError::CommandError(
            "Usage: calc <expression>".to_string(),
        ));
    }
    Ok(true)
}

#[inline]
fn handle_eval_command(parts: &[&str], state: &mut ReplState) -> ReplResult<bool> {
    if parts.len() >= 2 {
        let expr = parts[1..].join(" ");
        let expanded = state.expand_variables(&expr);
        println!("🧮 Evaluating: {}", expanded);
        println!("   (Full expression evaluation not implemented)");
        println!("   Try 'calc' for simple arithmetic");
    } else {
        return Err(ReplError::CommandError(
            "Usage: eval <expression>".to_string(),
        ));
    }
    Ok(true)
}

#[inline]
fn display_history(state: &ReplState) {
    println!("📜 Command History:");
    if state.history.is_empty() {
        println!("  (No commands in history yet)");
    } else {
        for (i, cmd) in state.history.iter().rev().take(20).enumerate() {
            println!("  {:3}: {}", state.history.len() - i, cmd);
        }
        if state.history.len() > 20 {
            println!("  ... ({} more entries)", state.history.len() - 20);
        }
    }
    println!("\n  Use Ctrl+P/Up and Ctrl+N/Down to navigate");
}

#[inline]
fn display_all_variables(state: &ReplState) {
    println!("📋 Custom variables:");
    for (name, value) in &state.variables {
        println!("  ${} = \"{}\"", name, value);
    }
    println!("\n📋 Environment variables (first 10):");
    for (_, (key, value)) in env::vars().enumerate().take(10) {
        println!("  ${} = \"{}\"", key, value);
    }
    println!("  ... and {} more", env::vars().count().saturating_sub(10));
}

#[inline]
fn display_unicode_test() {
    println!("🦀 Unicode Test Suite:");
    println!("  Emoji: 😀 🎉 🚀 ❤️ 🌟");
    println!("  Chinese: 你好世界 (Hello World)");
    println!("  Japanese: こんにちは世界 (Hello World)");
    println!("  Korean: 안녕하세요 (Hello)");
    println!("  Arabic: مرحبا بالعالم (Hello World)");
    println!("  Hebrew: שלום עולם (Hello World)");
    println!("  Russian: Привет мир (Hello World)");
    println!("  Greek: Γεια σου κόσμε (Hello World)");
    println!("  Math: ∫ ∑ ∏ √ ∞ ≈ ≠ ≤ ≥ ∀ ∃ ∈ ∉");
    println!("  Symbols: © ® ™ € £ ¥ § ¶ † ‡ • ★");
    println!("  Box Drawing: ┌─┬─┐ │ │ │ ├─┼─┤ └─┴─┘");
}

#[inline]
fn display_config(state: &ReplState) {
    println!("⚙️  Rustline Configuration:");
    println!("  History Max Size: 1024");
    println!("  Hints: Enabled");
    println!("  Completion: Enabled (Tab)");
    println!("  Multiline: Enabled");
    println!("  Bracketed Paste: Enabled");
    println!("  Balance Pairs: Disabled");
    println!("\n🎨 Current State:");
    println!("  Commands executed: {}", state.command_count);
    println!("  Variables defined: {}", state.variables.len());
    println!("  Aliases defined: {}", state.aliases.len());
    println!("  History entries: {}", state.history.len());
    println!(
        "  Multiline mode: {}",
        if state.multiline_mode { "ON" } else { "OFF" }
    );
    println!(
        "  Debug mode: {}",
        if state.debug_mode { "ON" } else { "OFF" }
    );
}

#[inline]
fn evaluate_expression(expr: &str) -> Result<f64, String> {
    let tokens: Vec<&str> = expr.split_whitespace().collect();

    if tokens.len() != 3 {
        return Err("Expected format: number operator number".to_string());
    }

    let left: f64 = tokens[0]
        .parse()
        .map_err(|_| format!("Invalid number: {}", tokens[0]))?;
    let right: f64 = tokens[2]
        .parse()
        .map_err(|_| format!("Invalid number: {}", tokens[2]))?;

    match tokens[1] {
        "+" => Ok(left + right),
        "-" => Ok(left - right),
        "*" => Ok(left * right),
        "/" => {
            if right == 0.0 {
                Err("Division by zero".to_string())
            } else {
                Ok(left / right)
            }
        }
        "%" => Ok(left % right),
        "^" | "**" => Ok(left.powf(right)),
        op => Err(format!("Unknown operator: {}", op)),
    }
}

fn run_micro_benchmarks(state: &ReplState) {
    use std::time::Instant;

    println!("🏃 Running micro benchmarks...");

    let test_str = "Hello $USER from $HOME";
    let start = Instant::now();
    for _ in 0..10000 {
        let _ = state.expand_variables(test_str);
    }
    let duration = start.elapsed();
    println!("  Variable expansion (10k iterations): {:?}", duration);

    let test_cmd = "ls -la";
    let start = Instant::now();
    for _ in 0..10000 {
        let _ = state.expand_aliases(test_cmd);
    }
    let duration = start.elapsed();
    println!("  Alias expansion (10k iterations): {:?}", duration);

    let start = Instant::now();
    for _ in 0..10000 {
        let _ = evaluate_expression("42 + 42");
    }
    let duration = start.elapsed();
    println!("  Expression evaluation (10k iterations): {:?}", duration);
}

fn print_help() {
    println!(
        r#"
🦀 Rustline REPL - Feature Showcase
===================================

📋 Available Commands:
  help          - Show this help message
  exit/quit     - Exit the REPL
  history       - Show command history
  clear         - Clear the screen
  set X Y       - Set variable X to value Y
  get [X]       - Get variable X or show all variables
  echo TEXT     - Echo text with $variable expansion
  pwd           - Print working directory
  cd [DIR]      - Change directory (default: home)
  ls [DIR]      - List directory contents with colors
  cat FILE      - Display file contents
  alias N=CMD   - Create command alias
  unalias N     - Remove alias
  calc EXPR     - Simple calculator (e.g., calc 2 + 2)
  multiline     - Toggle multiline mode
  unicode-test  - Display Unicode test characters
  config        - Show configuration and state
  eval EXPR     - Evaluate expression (demo)
  debug         - Toggle debug mode
  bench         - Run micro benchmarks

⌨️  Keyboard Shortcuts:
  Ctrl+A/E      - Move to start/end of line
  Ctrl+B/F      - Move left/right (or arrow keys)
  Alt+B/F       - Move by word
  Ctrl+H        - Backspace
  Ctrl+D        - Delete or EOF
  Ctrl+K        - Kill to end of line
  Ctrl+U        - Kill to start of line
  Ctrl+W        - Delete word backward
  Alt+D         - Delete word forward
  Ctrl+Y        - Yank (paste) from kill ring
  Alt+Y         - Rotate kill ring and yank
  Ctrl+L        - Clear screen
  Ctrl+P/N      - Previous/next history (or arrows)
  Ctrl+C        - Cancel current line
  Tab           - Completion

🎯 Features Demonstrated:
  • Tab completion for commands, files, and env vars
  • Command history with navigation
  • Kill ring (cut/paste) operations
  • Unicode and emoji support
  • Variable expansion with $name (custom and env)
  • Command aliases
  • Simple calculator
  • Multiline editing
  • Context-aware completion
  • Bracketed paste mode support
  • Performance benchmarking
  • Debug mode for diagnostics

💡 Tips:
  • Use Tab to complete commands and file paths
  • Try $HOME, $USER, $PATH to see environment variables
  • Create aliases for frequently used commands
  • Use the kill ring for advanced text manipulation
  • Enable debug mode to see internal operations
"#
    );
}

fn print_banner() {
    println!(
        r#"
╔═══════════════════════════════════════════════════════════╗
║     🦀 Rustline REPL - Advanced Feature Demonstration     ║
╠═══════════════════════════════════════════════════════════╣
║  A comprehensive showcase of Rustline's capabilities      ║
║  Type 'help' for commands | 'exit' to quit               ║
╚═══════════════════════════════════════════════════════════╝
"#
    );
}

#[inline]
fn process_multiline(
    line: &str,
    multiline_buffer: &mut Vec<String>,
    state: &mut ReplState,
) -> bool {
    if line.trim().is_empty() {
        let full_command = multiline_buffer.join("\n");
        multiline_buffer.clear();

        if let Err(e) = process_command(&full_command, state) {
            print_error(&e, state.debug_mode);
        }
        true
    } else {
        multiline_buffer.push(line.to_string());
        true
    }
}

#[inline]
fn print_error(error: &ReplError, debug_mode: bool) {
    eprintln!("❌ Error: {}", error);
    if debug_mode {
        eprintln!("   Debug: {:?}", error);
    }
}

#[inline]
fn process_command(command: &str, state: &mut ReplState) -> Result<(), ReplError> {
    match execute_command(command, state) {
        Ok(false) => std::process::exit(0),
        Ok(true) => Ok(()),
        Err(e) => Err(e),
    }
}

#[inline]
fn process_multiline_eof(multiline_buffer: &mut Vec<String>, state: &mut ReplState) -> bool {
    if !multiline_buffer.is_empty() {
        let full_command = multiline_buffer.join("\n");
        if let Err(e) = process_command(&full_command, state) {
            print_error(&e, state.debug_mode);
        }
        multiline_buffer.clear();
        true
    } else {
        println!("\n👋 EOF detected. Goodbye!");
        false
    }
}

fn main() {
    print_banner();

    let config = Config {
        history_max_size: 1024,
        enable_hints: true,
        enable_completion: true,
        enable_multiline: true,
        enable_bracketed_paste: true,
        balance_pairs: false,
    };

    let mut rl = Rustline::with_config(config);

    let completer = Box::new(SmartCompleter::new());
    rl.set_completer(completer);

    let mut state = ReplState::new();
    let mut multiline_buffer = Vec::new();

    loop {
        let prompt = if multiline_buffer.is_empty() {
            format!("rustline:{}> ", state.command_count + 1)
        } else {
            "... ".to_string()
        };

        match rl.readline(&prompt) {
            Ok(line) => {
                if !line.trim().is_empty() && multiline_buffer.is_empty() {
                    state.history.push(line.clone());
                }

                if state.multiline_mode
                    && !multiline_buffer.is_empty()
                    && process_multiline(&line, &mut multiline_buffer, &mut state)
                {
                    continue;
                }

                if state.multiline_mode && line.trim().ends_with('{') {
                    multiline_buffer.push(line.to_string());
                    continue;
                }

                if let Err(e) = process_command(&line, &mut state) {
                    print_error(&e, state.debug_mode);
                }
            }
            Err(rustline::RustlineError::Interrupted) => {
                println!("\n⚡ Interrupted! Use 'exit' to quit.");
                multiline_buffer.clear();
            }
            Err(rustline::RustlineError::Eof) => {
                if !process_multiline_eof(&mut multiline_buffer, &mut state) {
                    break;
                }
            }
            Err(err) => {
                eprintln!("❌ Readline error: {:?}", err);
                break;
            }
        }
    }

    println!("\n🦀 Thanks for using Rustline REPL!");
    println!("   Commands executed: {}", state.command_count);

    if state.debug_mode {
        println!("\n📊 Debug Statistics:");
        println!("   Peak variables: {}", state.variables.capacity());
        println!("   Peak aliases: {}", state.aliases.capacity());
        println!("   History size: {}", state.history.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_variable_expansion() {
        let mut state = ReplState::new();
        state
            .variables
            .insert("TEST".to_string(), "value".to_string());

        let result = state.expand_variables("Hello $TEST");
        assert_eq!(result, "Hello value");

        let result = state.expand_variables("No variables here");
        assert_eq!(result, "No variables here");
    }

    #[test]
    fn test_alias_expansion() {
        let mut state = ReplState::new();
        state.aliases.insert("ll".to_string(), "ls -la".to_string());

        let result = state.expand_aliases("ll /tmp");
        assert_eq!(result, "ls -la /tmp");

        let result = state.expand_aliases("ls /tmp");
        assert_eq!(result, "ls /tmp");
    }

    #[test]
    fn test_expression_evaluation() {
        assert_eq!(evaluate_expression("2 + 2").unwrap(), 4.0);
        assert_eq!(evaluate_expression("10 * 5").unwrap(), 50.0);
        assert_eq!(evaluate_expression("100 / 4").unwrap(), 25.0);
        assert_eq!(evaluate_expression("2 ^ 3").unwrap(), 8.0);

        assert!(evaluate_expression("10 / 0").is_err());
        assert!(evaluate_expression("invalid").is_err());
    }
}
