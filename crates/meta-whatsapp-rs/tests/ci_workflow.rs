//! The CI workflow runs everything `just ci` runs.
//!
//! `.github/workflows/ci.yml` splits the gate into parallel jobs, each
//! running `just <recipe>` steps. Nothing but this test keeps a recipe added
//! to the justfile's `ci` list from being left out of every job, so it reads
//! both: the union of the jobs' recipes must equal `ci`'s dependencies, and
//! the job named `just ci` (the required check) must need every other job
//! and run even when one fails. The workflow is read line by line, as the
//! file's header comment promises: a step is `- run: just <recipe>` and
//! `needs:` is a one-line list.

#![allow(clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The recipes `ci` depends on, from the justfile's `ci: a b c` line.
fn ci_recipes(justfile: &str) -> BTreeSet<String> {
    let line = justfile
        .lines()
        .find(|l| l.starts_with("ci:"))
        .expect("the justfile has no `ci:` recipe");
    let deps: BTreeSet<String> = line["ci:".len()..]
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    assert!(!deps.is_empty(), "`ci:` has no dependencies: {line}");
    deps
}

#[derive(Debug, Default)]
struct Job {
    id: String,
    name: Option<String>,
    needs: Vec<String>,
    if_always: bool,
    recipes: Vec<String>,
}

/// The workflow's jobs: id, display name, `needs`, `if`, and `just` steps.
fn jobs(workflow: &str) -> Vec<Job> {
    let mut jobs: Vec<Job> = Vec::new();
    let mut in_jobs = false;
    for raw in workflow.lines() {
        let line = raw.trim_end();
        if line == "jobs:" {
            in_jobs = true;
            continue;
        }
        if !in_jobs || line.trim_start().starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if !line.starts_with(' ') {
            in_jobs = false;
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let text = line.trim();
        if indent == 2 {
            if let Some(id) = text.strip_suffix(':') {
                jobs.push(Job {
                    id: id.to_owned(),
                    ..Job::default()
                });
            }
            continue;
        }
        let Some(job) = jobs.last_mut() else { continue };
        if indent == 4 {
            if let Some(n) = text.strip_prefix("name:") {
                job.name = Some(n.trim().to_owned());
            } else if let Some(n) = text.strip_prefix("needs:") {
                let list = n.trim().trim_start_matches('[').trim_end_matches(']');
                job.needs = list
                    .split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect();
            } else if text
                .strip_prefix("if:")
                .is_some_and(|c| c.trim() == "always()")
            {
                job.if_always = true;
            }
        } else if let Some(args) = text
            .strip_prefix("- run:")
            .and_then(|cmd| cmd.trim().strip_prefix("just "))
        {
            job.recipes.push(args.trim().to_owned());
        }
    }
    jobs
}

#[test]
fn the_jobs_run_every_recipe_of_just_ci() {
    let want = ci_recipes(&read("justfile"));
    let all = jobs(&read(".github/workflows/ci.yml"));
    let got: BTreeSet<String> = all.iter().flat_map(|j| j.recipes.clone()).collect();
    assert_eq!(
        got, want,
        "the workflow's `just <recipe>` steps must be exactly `just ci`'s recipes"
    );
    for j in &all {
        for r in &j.recipes {
            assert!(
                !r.contains(char::is_whitespace) && r != "ci",
                "job {}: `just {r}` must be one recipe of `ci`, not `ci` or a reconstruction",
                j.id
            );
        }
    }
}

#[test]
fn the_required_check_needs_every_other_job() {
    let all = jobs(&read(".github/workflows/ci.yml"));
    let gate: Vec<&Job> = all
        .iter()
        .filter(|j| j.name.as_deref() == Some("just ci"))
        .collect();
    assert_eq!(gate.len(), 1, "exactly one job is named `just ci`");
    let gate = gate[0];
    assert!(
        gate.if_always,
        "the `just ci` job needs `if: always()`: a skipped required check passes"
    );
    let others: BTreeSet<&str> = all
        .iter()
        .filter(|j| j.id != gate.id)
        .map(|j| j.id.as_str())
        .collect();
    let needs: BTreeSet<&str> = gate.needs.iter().map(String::as_str).collect();
    assert_eq!(needs, others, "the `just ci` job must need every other job");
    assert!(gate.recipes.is_empty(), "the `just ci` job runs no recipe");
}
