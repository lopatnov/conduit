pub mod args;
pub mod init;

/// A CLI subcommand that can be executed.
///
/// ## Adding a new command
///
/// 1. Add a variant to `Command` in `cli/args.rs`.
/// 2. Create a struct (in `main.rs` or a dedicated module) that holds the
///    pre-extracted arguments for that command.
/// 3. `impl CliCommand for YourCmd { fn execute(self) { ... } }`.
/// 4. Add one arm to `dispatch_command()` in `main.rs`.
///
/// No other changes to `main()` are required.
pub trait CliCommand {
    /// Run the command.  Implementations may call `std::process::exit` on
    /// fatal errors (consistent with the binary entry-point convention).
    fn execute(self);
}
