//! The shipped eval suites (`evals/tasks`, `evals/hard`): every task parses, its checks fail
//! on the files it starts with, and pass on its reference solution (`solution/`: files to
//! put in place, `ANSWER` for the reply, `DELETE` listing files to remove).

use std::path::{Path, PathBuf};

use quack_exec::eval::{check_workspace, copy_dir, load_tasks};

fn suite(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals")
        .join(name)
}

fn fail_as_given_and_pass_when_solved(name: &str, at_least: usize) {
    let tasks = load_tasks(&suite(name), &[]).expect("suite");
    assert!(tasks.len() >= at_least, "{name}: {} tasks", tasks.len());
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut checked = 0;
    for task in &tasks {
        assert!(!task.file.checks.is_empty(), "{}: no checks", task.id);
        assert!(
            task.dir.join("repo").is_dir() || task.file.base.is_some(),
            "{}: no repo/ or base",
            task.id
        );
        let solution = task.dir.join("solution");
        assert!(solution.is_dir(), "{}: no solution/", task.id);
        let tools: Vec<String> = task
            .file
            .checks
            .iter()
            .filter_map(|c| c.tool_called.clone())
            .collect();

        let ws = tmp.path().join(&task.id);
        task.prepare(&ws).expect("copy");
        let before = match check_workspace(task, &ws, "", &[]) {
            Ok(f) => f,
            // A program the task needs is missing here; CI has them.
            Err(_) => continue,
        };
        assert!(!before.is_empty(), "{}: passes before any change", task.id);

        // Solve it from scratch, so nothing the checks left behind counts.
        std::fs::remove_dir_all(&ws).expect("clean");
        task.prepare(&ws).expect("copy");
        let read = |name: &str| std::fs::read_to_string(solution.join(name)).unwrap_or_default();
        for path in read("DELETE").lines().filter(|l| !l.trim().is_empty()) {
            std::fs::remove_file(ws.join(path.trim())).expect("delete");
        }
        copy_dir(&solution, &ws).expect("solution");
        for marker in ["ANSWER", "DELETE"] {
            let _ = std::fs::remove_file(ws.join(marker));
        }
        let after = check_workspace(task, &ws, &read("ANSWER"), &tools).expect("checks");
        assert!(after.is_empty(), "{}: solution fails: {after:?}", task.id);
        checked += 1;
    }
    assert!(checked > 0);
}

#[test]
fn tasks_fail_as_given_and_pass_when_solved() {
    fail_as_given_and_pass_when_solved("tasks", 20);
}

#[test]
fn hard_tasks_fail_as_given_and_pass_when_solved() {
    fail_as_given_and_pass_when_solved("hard", 15);
}
