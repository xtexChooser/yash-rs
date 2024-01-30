// This file is part of yash, an extended POSIX shell.
// Copyright (C) 2024 WATANABE Yuki
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

//! Implementation of `Command::Send`
//!
//! [`execute`] calls [`send`] for each target and reports all errors.
//! [`send`] uses [`resolve_target`] to determine the argument to the
//! [`kill`](yash_env::System::kill) system call.
use crate::common::{report_failure, to_single_message};
use std::borrow::Cow;
use std::num::ParseIntError;
use thiserror::Error;
use yash_env::job::id::parse_tail;
use yash_env::job::Pid;
use yash_env::job::{id::FindError, JobSet};
use yash_env::semantics::Field;
use yash_env::system::Errno;
use yash_env::trap::Signal;
use yash_env::Env;
use yash_env::System as _;
use yash_syntax::source::pretty::{Annotation, AnnotationType, MessageBase};

/// Error that may occur while [sending](send) a signal.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum Error {
    /// The specified process (group) ID was not a valid integer.
    #[error(transparent)]
    ProcessId(#[from] ParseIntError),
    /// The specified job ID did not uniquely identify a job.
    #[error(transparent)]
    JobId(#[from] FindError),
    /// The job ID specifies a job that is not job-controlled.
    #[error("target job is not job-controlled")]
    Unmonitored,
    /// An error occurred in the underlying system call.
    #[error(transparent)]
    System(#[from] Errno),
}

/// Resolves the specified target into a process (group) ID.
///
/// The target may be specified as a job ID, a process ID, or a process group
/// ID. In case of a process group ID, the value should be negative.
pub fn resolve_target(jobs: &JobSet, target: &str) -> Result<Pid, Error> {
    if let Some(tail) = target.strip_prefix('%') {
        let job_id = parse_tail(tail);
        let index = job_id.find(jobs)?;
        let job = &jobs[index];
        if job.job_controlled {
            Ok(-job.pid)
        } else {
            Err(Error::Unmonitored)
        }
    } else {
        Ok(Pid(target.parse()?))
    }
}

/// Sends the specified signal to the specified target.
pub async fn send(env: &mut Env, signal: Option<Signal>, target: &Field) -> Result<(), Error> {
    let pid = resolve_target(&env.jobs, &target.value)?;
    env.system.kill(pid, signal).await?;
    Ok(())
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{target}: {error}")]
struct TargetError<'a> {
    target: &'a Field,
    error: Error,
}

impl MessageBase for TargetError<'_> {
    fn message_title(&self) -> Cow<str> {
        "cannot send signal".into()
    }

    fn main_annotation(&self) -> Annotation<'_> {
        Annotation::new(
            AnnotationType::Error,
            self.to_string().into(),
            &self.target.origin,
        )
    }
}

/// Executes the `Send` command.
pub async fn execute(env: &mut Env, signal: Option<Signal>, targets: &[Field]) -> crate::Result {
    let mut errors = Vec::new();
    for target in targets {
        if let Err(error) = send(env, signal, target).await {
            errors.push(TargetError { target, error });
        }
    }

    if let Some(message) = to_single_message(&{ errors }) {
        report_failure(env, message).await
    } else {
        crate::Result::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use yash_env::job::Job;

    #[test]
    fn resolve_target_process_ids() {
        let jobs = JobSet::new();

        let result = resolve_target(&jobs, "123");
        assert_eq!(result, Ok(Pid(123)));

        let result = resolve_target(&jobs, "-456");
        assert_eq!(result, Ok(Pid(-456)));
    }

    #[test]
    fn resolve_target_job_id() {
        let mut jobs = JobSet::new();
        let mut job = Job::new(Pid(123));
        job.job_controlled = true;
        job.name = "my job".into();
        jobs.add(job);

        let result = resolve_target(&jobs, "%my");
        assert_eq!(result, Ok(Pid(-123)));
    }

    #[test]
    fn resolve_target_job_find_error() {
        let jobs = JobSet::new();
        let result = resolve_target(&jobs, "%my");
        assert_eq!(result, Err(Error::JobId(FindError::NotFound)));
    }

    #[test]
    fn resolve_target_unmonitored() {
        let mut jobs = JobSet::new();
        let mut job = Job::new(Pid(123));
        job.job_controlled = false;
        job.name = "my job".into();
        jobs.add(job);

        let result = resolve_target(&jobs, "%my");
        assert_eq!(result, Err(Error::Unmonitored));
    }

    #[test]
    fn resolve_target_invalid_string() {
        let jobs = JobSet::new();
        let result = resolve_target(&jobs, "abc");
        assert_matches!(result, Err(Error::ProcessId(_)));
    }
}
