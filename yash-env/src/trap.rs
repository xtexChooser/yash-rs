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

//! Type definitions for signal handling settings.

use crate::system::{Errno, SignalHandling};
#[cfg(doc)]
use crate::system::{SharedSystem, System};

#[doc(no_inline)]
pub use nix::sys::signal::Signal;

/// System interface for signal handling configuration.
pub trait SignalSystem {
    /// Sets how a signal is handled.
    ///
    /// This function updates the signal blocking mask and the signal action for
    /// the specified signal and remembers the previous configuration for
    /// restoration.
    ///
    /// Returns the previous handler.
    fn set_signal_handling(
        &mut self,
        signal: Signal,
        handling: SignalHandling,
    ) -> Result<SignalHandling, Errno>;
}

/// Collection of signal handling settings.
///
/// A `TrapSet` remembers the trap configured for each signal and manages the
/// signal handlers installed to the underlying system. `TrapSet` acts as a
/// decorator for a system implementing [`SignalSystem`]. Methods of `TrapSet`
/// expect to be passed the same system instance in every call.
///
/// `TrapSet` manages two types of signal handling configurations. One is
/// user-defined traps, which are explicitly configured by the trap built-in.
/// The other is internal handlers, which are installed by the internals of the
/// shell to implement additional actions the shell is required to perform.
///
/// `TrapSet` merges the two configurations into a single
/// [`system::SignalHandling`](SignalHandling) for each signal and sets it to
/// the system.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TrapSet {
    initial_sigchld: Option<SignalHandling>,
}

// TODO Support user-defined traps
// TODO Extend internal handlers for other signals
impl TrapSet {
    /// Installs an internal handler for `SIGCHLD`.
    ///
    /// You should install the `SIGCHLD` handler to the system by using this
    /// function before waiting for `SIGCHLD` with [`System::wait`] and
    /// [`SharedSystem::wait_for_signal`].
    ///
    /// This function remembers that the handler has been installed, so a second
    /// call to the function will be a no-op.
    pub fn enable_sigchld_handler<S: SignalSystem>(&mut self, system: &mut S) -> Result<(), Errno> {
        let previous_handler =
            system.set_signal_handling(Signal::SIGCHLD, SignalHandling::Catch)?;
        self.initial_sigchld.get_or_insert(previous_handler);
        Ok(())
    }

    /// Uninstalls all internal handlers.
    ///
    /// This function removes all internal handlers that have been previously
    /// installed by `self`. It leaves handlers for any existing user-defined
    /// traps.
    pub fn disable_internal_handlers<S: SignalSystem>(
        &mut self,
        system: &mut S,
    ) -> Result<(), Errno> {
        if let Some(initial_handler) = self.initial_sigchld {
            system
                .set_signal_handling(Signal::SIGCHLD, initial_handler)
                .map(drop)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct DummySystem(HashMap<Signal, SignalHandling>);

    impl SignalSystem for DummySystem {
        fn set_signal_handling(
            &mut self,
            signal: Signal,
            handling: SignalHandling,
        ) -> Result<SignalHandling, Errno> {
            Ok(self
                .0
                .insert(signal, handling)
                .unwrap_or(SignalHandling::Default))
        }
    }

    #[test]
    fn enabling_sigchld_handler() {
        let mut system = DummySystem::default();
        let mut trap_set = TrapSet::default();
        trap_set.enable_sigchld_handler(&mut system).unwrap();
        assert_eq!(system.0[&Signal::SIGCHLD], SignalHandling::Catch);
    }

    #[test]
    fn disabling_internal_handler_for_initially_defaulted_sigchld() {
        let mut system = DummySystem::default();
        let mut trap_set = TrapSet::default();
        trap_set.enable_sigchld_handler(&mut system).unwrap();
        trap_set.disable_internal_handlers(&mut system).unwrap();
        assert_eq!(system.0[&Signal::SIGCHLD], SignalHandling::Default);
    }

    #[test]
    fn disabling_internal_handler_for_initially_ignored_sigchld() {
        let mut system = DummySystem::default();
        system.0.insert(Signal::SIGCHLD, SignalHandling::Ignore);
        let mut trap_set = TrapSet::default();
        trap_set.enable_sigchld_handler(&mut system).unwrap();
        trap_set.disable_internal_handlers(&mut system).unwrap();
        assert_eq!(system.0[&Signal::SIGCHLD], SignalHandling::Ignore);
    }

    #[test]
    fn disabling_internal_handler_after_enabling_twice() {
        let mut system = DummySystem::default();
        system.0.insert(Signal::SIGCHLD, SignalHandling::Ignore);
        let mut trap_set = TrapSet::default();
        trap_set.enable_sigchld_handler(&mut system).unwrap();
        trap_set.enable_sigchld_handler(&mut system).unwrap();
        trap_set.disable_internal_handlers(&mut system).unwrap();
        assert_eq!(system.0[&Signal::SIGCHLD], SignalHandling::Ignore);
    }

    #[test]
    fn disabling_internal_handler_without_enabling() {
        let mut system = DummySystem::default();
        system.0.insert(Signal::SIGCHLD, SignalHandling::Ignore);
        let mut trap_set = TrapSet::default();
        trap_set.disable_internal_handlers(&mut system).unwrap();
        assert_eq!(system.0[&Signal::SIGCHLD], SignalHandling::Ignore);
    }
}
