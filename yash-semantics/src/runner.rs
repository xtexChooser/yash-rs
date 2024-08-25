// This file is part of yash, an extended POSIX shell.
// Copyright (C) 2021 WATANABE Yuki
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Implementation of the read-eval loop

use crate::command::Command;
use crate::trap::run_traps_for_caught_signals;
use crate::Handle;
use std::cell::RefCell;
use std::ops::ControlFlow::{Break, Continue};
use yash_env::semantics::Divert;
use yash_env::semantics::ExitStatus;
use yash_env::semantics::Result;
use yash_env::Env;
use yash_syntax::parser::lex::Lexer;
use yash_syntax::parser::Parser;
use yash_syntax::syntax::List;

/// Reads input, parses it, and executes commands in a loop.
///
/// A read-eval loop uses a [`Lexer`] for reading and parsing input and [`Env`]
/// for executing parsed commands. It creates a [`Parser`] from the lexer to
/// parse [command lines](Parser::command_line). The loop executes each command
/// line before parsing the next one. The loop continues until the parser
/// reaches the end of input or encounters a parser error, or the command
/// execution results in a `Break(Divert::...)`.
///
/// This function takes a `RefCell` containing the mutable reference to the
/// environment. The `RefCell` should be shared only with the [`Input`]
/// implementor used in the `Lexer` to avoid conflicting borrows.
///
/// If the input source code contains no commands, the exit status is set to
/// zero. Otherwise, the exit status reflects the result of the last executed
/// command.
///
/// [Pending traps are run](run_traps_for_caught_signals) and [subshell statuses
/// are updated](Env::update_all_subshell_statuses) between parsing input and
/// running commands.
///
/// For the top-level read-eval loop of an interactive shell, see
/// [`interactive_read_eval_loop`].
///
/// # Example
///
/// Executing a command:
///
/// ```
/// # futures_executor::block_on(async {
/// # use std::cell::RefCell;
/// # use std::ops::ControlFlow::Continue;
/// # use yash_env::Env;
/// # use yash_semantics::ExitStatus;
/// # use yash_semantics::read_eval_loop;
/// # use yash_syntax::parser::lex::Lexer;
/// # use yash_syntax::source::Source;
/// let mut env = Env::new_virtual();
/// let mut lexer = Lexer::from_memory("case foo in (bar) ;; esac", Source::Unknown);
/// let result = read_eval_loop(&RefCell::new(&mut env), &mut lexer).await;
/// assert_eq!(result, Continue(()));
/// assert_eq!(env.exit_status, ExitStatus::SUCCESS);
/// # })
/// ```
///
/// Using the [`Echo`] decorator with the shared environment:
///
/// ```
/// # futures_executor::block_on(async {
/// # use std::cell::RefCell;
/// # use std::num::NonZeroU64;
/// # use std::ops::ControlFlow::Continue;
/// # use std::rc::Rc;
/// # use yash_env::Env;
/// # use yash_env::input::Echo;
/// # use yash_semantics::ExitStatus;
/// # use yash_semantics::read_eval_loop;
/// # use yash_syntax::input::Memory;
/// # use yash_syntax::parser::lex::Lexer;
/// # use yash_syntax::source::Source;
/// let mut env = Env::new_virtual();
/// let mut ref_env = RefCell::new(&mut env);
/// let input = Box::new(Echo::new(Memory::new("case foo in (bar) ;; esac"), &ref_env));
/// let mut lexer = Lexer::new(input, NonZeroU64::MIN, Rc::new(Source::CommandString));
/// let result = read_eval_loop(&ref_env, &mut lexer).await;
/// drop(lexer);
/// assert_eq!(result, Continue(()));
/// assert_eq!(env.exit_status, ExitStatus::SUCCESS);
/// # })
/// ```
///
/// [`Echo`]: yash_env::input::Echo
/// [`Input`]: yash_syntax::input::Input
pub async fn read_eval_loop(env: &RefCell<&mut Env>, lexer: &mut Lexer<'_>) -> Result {
    read_eval_loop_impl(env, lexer, /* is_interactive */ false).await
}

/// [`read_eval_loop`] for interactive shells
///
/// This function extends the [`read_eval_loop`] function to act as an
/// interactive shell. The difference is that this function suppresses
/// [`Interrupt`]s and continues the loop if the parser fails with a syntax
/// error or if the command execution results in an interrupt. Note that I/O
/// errors detected by the parser are not recovered from.
///
/// Also note that the following aspects of the interactive shell are *not*
/// implemented in this function:
///
/// - Prompting the user for input (see the `yash-prompt` crate)
/// - Reporting job status changes before the prompt
/// - Applying the [`IgnoreEof`] option
///
/// This function is intended to be used as the top-level read-eval loop in an
/// interactive shell. It is not suitable for non-interactive command execution
/// such as scripts. See [`read_eval_loop`] for non-interactive execution.
///
/// [`Interrupt`]: crate::Divert::Interrupt
/// [`IgnoreEof`]: yash_env::option::IgnoreEof
pub async fn interactive_read_eval_loop(env: &RefCell<&mut Env>, lexer: &mut Lexer<'_>) -> Result {
    read_eval_loop_impl(env, lexer, /* is_interactive */ true).await
}

// The RefCell should be local to the loop, so it is safe to keep the mutable
// borrow across await points.
#[allow(clippy::await_holding_refcell_ref)]
async fn read_eval_loop_impl(
    env: &RefCell<&mut Env>,
    lexer: &mut Lexer<'_>,
    is_interactive: bool,
) -> Result {
    let mut executed = false;

    loop {
        if !lexer.pending() {
            lexer.flush();
        }

        let command = Parser::new(lexer, env).command_line().await;

        let env = &mut **env.borrow_mut();
        match command {
            // No more commands
            Ok(None) => {
                if !executed {
                    env.exit_status = ExitStatus::SUCCESS;
                }
                return Continue(());
            }

            // Execute the command
            Ok(Some(command)) => {
                let mut result = run_command(env, &command).await;

                if is_interactive {
                    // Recover from interrupts
                    if let Break(Divert::Interrupt(exit_status)) = result {
                        if let Some(exit_status) = exit_status {
                            env.exit_status = exit_status;
                        }
                        result = Continue(());
                    }
                }

                result?
            }

            // Syntax error
            Err(error) => error.handle(env).await?,
        };
        executed = true;
    }
}

async fn run_command(env: &mut Env, command: &List) -> Result {
    run_traps_for_caught_signals(env).await?;
    env.update_all_subshell_statuses();
    command.execute(env).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::echo_builtin;
    use crate::tests::return_builtin;
    use futures_util::FutureExt;
    use std::num::NonZeroU64;
    use std::rc::Rc;
    use yash_env::input::Echo;
    use yash_env::input::Memory;
    use yash_env::option::Option::Verbose;
    use yash_env::option::State::On;
    use yash_env::system::r#virtual::VirtualSystem;
    use yash_env::system::r#virtual::SIGUSR1;
    use yash_env::trap::Action;
    use yash_env_test_helper::assert_stderr;
    use yash_env_test_helper::assert_stdout;
    use yash_syntax::source::Location;
    use yash_syntax::source::Source;

    #[test]
    fn exit_status_zero_with_no_commands() {
        let mut env = Env::new_virtual();
        env.exit_status = ExitStatus(5);
        let mut lexer = Lexer::from_memory("", Source::Unknown);
        let ref_env = RefCell::new(&mut env);

        let result = read_eval_loop(&ref_env, &mut lexer).now_or_never().unwrap();
        assert_eq!(result, Continue(()));
        assert_eq!(env.exit_status, ExitStatus::SUCCESS);
    }

    #[test]
    fn exit_status_in_out() {
        let system = VirtualSystem::new();
        let state = Rc::clone(&system.state);
        let mut env = Env::with_system(Box::new(system));
        env.exit_status = ExitStatus(42);
        env.builtins.insert("echo", echo_builtin());
        env.builtins.insert("return", return_builtin());
        let mut lexer = Lexer::from_memory("echo $?; return -n 7", Source::Unknown);
        let ref_env = RefCell::new(&mut env);

        let result = read_eval_loop(&ref_env, &mut lexer).now_or_never().unwrap();
        assert_eq!(result, Continue(()));
        assert_eq!(env.exit_status, ExitStatus(7));
        assert_stdout(&state, |stdout| assert_eq!(stdout, "42\n"));
    }

    #[test]
    fn executing_many_lines_of_code() {
        let system = VirtualSystem::new();
        let state = Rc::clone(&system.state);
        let mut env = Env::with_system(Box::new(system));
        env.builtins.insert("echo", echo_builtin());
        let mut lexer = Lexer::from_memory("echo 1\necho 2\necho 3;", Source::Unknown);
        let ref_env = RefCell::new(&mut env);

        let result = read_eval_loop(&ref_env, &mut lexer).now_or_never().unwrap();
        assert_eq!(result, Continue(()));
        assert_stdout(&state, |stdout| assert_eq!(stdout, "1\n2\n3\n"));
    }

    #[test]
    fn parsing_with_aliases() {
        use yash_syntax::alias::{Alias, HashEntry};
        let system = VirtualSystem::new();
        let state = Rc::clone(&system.state);
        let mut env = Env::with_system(Box::new(system));
        env.aliases.insert(HashEntry(Rc::new(Alias {
            name: "echo".to_string(),
            replacement: "echo alias\necho ok".to_string(),
            global: false,
            origin: Location::dummy(""),
        })));
        env.builtins.insert("echo", echo_builtin());
        let mut lexer = Lexer::from_memory("echo", Source::Unknown);
        let ref_env = RefCell::new(&mut env);

        let result = read_eval_loop(&ref_env, &mut lexer).now_or_never().unwrap();
        assert_eq!(result, Continue(()));
        assert_eq!(env.exit_status, ExitStatus::SUCCESS);
        assert_stdout(&state, |stdout| assert_eq!(stdout, "alias\nok\n"));
    }

    #[test]
    fn verbose_option() {
        let system = VirtualSystem::new();
        let state = Rc::clone(&system.state);
        let mut env = Env::with_system(Box::new(system));
        env.options.set(Verbose, On);
        let ref_env = RefCell::new(&mut env);
        let input = Box::new(Echo::new(Memory::new("case _ in esac"), &ref_env));
        let line = NonZeroU64::new(1).unwrap();
        let mut lexer = Lexer::new(input, line, Rc::new(Source::CommandString));

        let result = read_eval_loop(&ref_env, &mut lexer).now_or_never().unwrap();
        drop(lexer);
        assert_eq!(result, Continue(()));
        assert_eq!(env.exit_status, ExitStatus::SUCCESS);
        assert_stderr(&state, |stderr| assert_eq!(stderr, "case _ in esac"));
    }

    #[test]
    fn command_interrupt_interactive() {
        // If the command execution results in an interrupt in interactive mode,
        // the loop continues
        let system = VirtualSystem::new();
        let state = Rc::clone(&system.state);
        let mut env = Env::with_system(Box::new(system));
        env.builtins.insert("echo", echo_builtin());
        let mut lexer = Lexer::from_memory("${X?}\necho $?\n", Source::Unknown);
        let ref_env = RefCell::new(&mut env);

        let result = interactive_read_eval_loop(&ref_env, &mut lexer)
            .now_or_never()
            .unwrap();
        assert_eq!(result, Continue(()));
        assert_stdout(&state, |stdout| assert_eq!(stdout, "2\n"));
    }

    #[test]
    fn command_other_divert_interactive() {
        // If the command execution results in a divert other than an interrupt in
        // interactive mode, the loop breaks
        let system = VirtualSystem::new();
        let state = Rc::clone(&system.state);
        let mut env = Env::with_system(Box::new(system));
        env.builtins.insert("echo", echo_builtin());
        env.builtins.insert("return", return_builtin());
        let mut lexer = Lexer::from_memory("return 123\necho $?\n", Source::Unknown);
        let ref_env = RefCell::new(&mut env);

        let result = interactive_read_eval_loop(&ref_env, &mut lexer)
            .now_or_never()
            .unwrap();
        assert_eq!(result, Break(Divert::Return(Some(ExitStatus(123)))));
        assert_stdout(&state, |stdout| assert_eq!(stdout, ""));
    }

    #[test]
    fn command_interrupt_non_interactive() {
        // If the command execution results in an interrupt in non-interactive mode,
        // the loop breaks
        let system = VirtualSystem::new();
        let state = Rc::clone(&system.state);
        let mut env = Env::with_system(Box::new(system));
        env.builtins.insert("echo", echo_builtin());
        let mut lexer = Lexer::from_memory("${X?}\necho $?\n", Source::Unknown);
        let ref_env = RefCell::new(&mut env);

        let result = read_eval_loop(&ref_env, &mut lexer).now_or_never().unwrap();
        assert_eq!(result, Break(Divert::Interrupt(Some(ExitStatus::ERROR))));
        assert_stdout(&state, |stdout| assert_eq!(stdout, ""));
    }

    #[test]
    fn handling_syntax_error() {
        let system = VirtualSystem::new();
        let state = Rc::clone(&system.state);
        let mut env = Env::with_system(Box::new(system));
        let mut lexer = Lexer::from_memory(";;", Source::Unknown);
        let ref_env = RefCell::new(&mut env);
        let result = read_eval_loop(&ref_env, &mut lexer).now_or_never().unwrap();
        assert_eq!(result, Break(Divert::Interrupt(Some(ExitStatus::ERROR))));
        assert_stderr(&state, |stderr| assert_ne!(stderr, ""));
    }

    #[test]
    fn syntax_error_aborts_loop() {
        let system = VirtualSystem::new();
        let state = Rc::clone(&system.state);
        let mut env = Env::with_system(Box::new(system));
        env.builtins.insert("echo", echo_builtin());
        let mut lexer = Lexer::from_memory(";;\necho !", Source::Unknown);
        let ref_env = RefCell::new(&mut env);

        let result = read_eval_loop(&ref_env, &mut lexer).now_or_never().unwrap();
        assert_eq!(result, Break(Divert::Interrupt(Some(ExitStatus::ERROR))));
        assert_stdout(&state, |stdout| assert_eq!(stdout, ""));
    }

    #[test]
    fn running_traps_between_parsing_and_executing() {
        let system = VirtualSystem::new();
        let state = Rc::clone(&system.state);
        let mut env = Env::with_system(Box::new(system.clone()));
        env.builtins.insert("echo", echo_builtin());
        env.traps
            .set_action(
                &mut env.system,
                SIGUSR1,
                Action::Command("echo USR1".into()),
                Location::dummy(""),
                false,
            )
            .unwrap();
        let _ = state
            .borrow_mut()
            .processes
            .get_mut(&system.process_id)
            .unwrap()
            .raise_signal(SIGUSR1);
        let mut lexer = Lexer::from_memory("echo $?", Source::Unknown);
        let ref_env = RefCell::new(&mut env);

        let result = read_eval_loop(&ref_env, &mut lexer).now_or_never().unwrap();
        assert_eq!(result, Continue(()));
        assert_eq!(env.exit_status, ExitStatus::SUCCESS);
        assert_stdout(&state, |stdout| assert_eq!(stdout, "USR1\n0\n"));
    }
}
