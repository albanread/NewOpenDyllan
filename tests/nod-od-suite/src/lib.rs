//! `nod-od-suite` — curated OpenDylan-flavoured fixture regression suite.
//!
//! The actual fixtures live in `fixtures/` and the runner lives in
//! `tests/run.rs`. This library crate exists only so Cargo treats
//! `tests/run.rs` as an integration test target.
//!
//! Each fixture is a small hand-written `.dylan` program in the spirit
//! of `opendylan-tests/sources/`, restricted to language features the
//! current compiler implements (no macros, no collections, no
//! conditions — those land in Sprints 17, 20, 19 respectively). The
//! curated set is intentionally narrow and is the substitute for
//! self-hosting that PLAN.md §2.7 commits to.

pub mod test_support {
	use std::fs::{self, File, OpenOptions};
	use std::io::Write;
	use std::path::{Path, PathBuf};
	use std::process::{Command, ExitStatus, Stdio};
	use std::thread;
	use std::time::{Duration, SystemTime, UNIX_EPOCH};

	pub struct LoggedOutput {
		pub status: ExitStatus,
		pub stdout: String,
		pub stderr: String,
		pub stdout_path: PathBuf,
		pub stderr_path: PathBuf,
		pub meta_path: PathBuf,
	}

	fn logs_root() -> PathBuf {
		let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
		manifest_dir
			.parent()
			.unwrap()
			.parent()
			.unwrap()
			.join("target")
			.join("test-logs")
			.join("nod-od-suite")
	}

	pub fn test_log_dir(test_name: &str) -> PathBuf {
		let dir = logs_root().join(slug(test_name));
		fs::create_dir_all(&dir).expect("create test log dir");
		dir
	}

	pub fn run_command_with_watchdog(
		test_name: &str,
		step: &str,
		timeout: Duration,
		command: &mut Command,
	) -> LoggedOutput {
		let run_dir = unique_run_dir(test_name, step);
		let stdout_path = run_dir.join("stdout.txt");
		let stderr_path = run_dir.join("stderr.txt");
		let meta_path = run_dir.join("meta.txt");
		let stdout_file = File::create(&stdout_path).expect("create stdout log");
		let stderr_file = File::create(&stderr_path).expect("create stderr log");
		let cwd = command
			.get_current_dir()
			.map(Path::to_path_buf)
			.unwrap_or_else(|| std::env::current_dir().expect("current dir"));
		let mut meta = OpenOptions::new()
			.create(true)
			.append(true)
			.open(&meta_path)
			.expect("create meta log");
		writeln!(meta, "cwd: {}", cwd.display()).expect("write meta cwd");
		writeln!(meta, "command: {command:?}").expect("write meta command");
		writeln!(meta, "timeout_seconds: {}", timeout.as_secs()).expect("write meta timeout");

		command.stdout(Stdio::from(stdout_file));
		command.stderr(Stdio::from(stderr_file));

		let mut child = command.spawn().expect("spawn child command");
		writeln!(meta, "pid: {}", child.id()).expect("write meta pid");
		let start = std::time::Instant::now();
		loop {
			if let Some(status) = child.try_wait().expect("poll child command") {
				return LoggedOutput {
					status,
					stdout: read_log(&stdout_path),
					stderr: read_log(&stderr_path),
					stdout_path,
					stderr_path,
					meta_path,
				};
			}
			if start.elapsed() >= timeout {
				kill_process_tree(child.id());
				let _ = child.kill();
				let _ = child.wait();
				panic!(
					"command timed out after {:?}; stdout: {}; stderr: {}; meta: {}",
					timeout,
					stdout_path.display(),
					stderr_path.display(),
					meta_path.display()
				);
			}
			thread::sleep(Duration::from_millis(100));
		}
	}

	fn unique_run_dir(test_name: &str, step: &str) -> PathBuf {
		let nanos = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.unwrap()
			.as_nanos();
		let dir = test_log_dir(test_name).join(format!("{}-{}", slug(step), nanos));
		fs::create_dir_all(&dir).expect("create run log dir");
		dir
	}

	fn read_log(path: &Path) -> String {
		match fs::read(path) {
			Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
			Err(err) => format!("<failed to read {}: {err}>", path.display()),
		}
	}

	fn kill_process_tree(pid: u32) {
		#[cfg(windows)]
		{
			let _ = Command::new("taskkill")
				.args(["/PID", &pid.to_string(), "/T", "/F"])
				.status();
		}
	}

	fn slug(s: &str) -> String {
		let mut out = String::with_capacity(s.len());
		let mut last_dash = false;
		for ch in s.chars() {
			let keep = ch.is_ascii_alphanumeric();
			if keep {
				out.push(ch.to_ascii_lowercase());
				last_dash = false;
			} else if !last_dash {
				out.push('-');
				last_dash = true;
			}
		}
		out.trim_matches('-').to_string()
	}
}
