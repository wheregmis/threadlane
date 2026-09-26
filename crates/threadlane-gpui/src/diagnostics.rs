//! Process evidence independent of session journals, including failures to write them.
use std::{
    backtrace::Backtrace,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct ProcessLog {
    file: Arc<File>,
}

impl ProcessLog {
    pub fn open() -> io::Result<Self> {
        let dir = threadlane_project::default_global_threadlane_dir()
            .ok_or_else(|| io::Error::other("cannot resolve Threadlane data directory"))?
            .join("logs");
        let (log, path) = Self::create(&dir)?;
        install_panic_hook(path.with_extension("panic.log"));
        Ok(log)
    }

    fn create(dir: &Path) -> io::Result<(Self, std::path::PathBuf)> {
        fs::create_dir_all(dir)?;
        // ponytail: retain per-process logs; add retention if disk usage becomes material.
        let path = dir.join(format!("app-{}-{}.log", now_ms(), std::process::id()));
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)?;
        let log = Self {
            file: Arc::new(file),
        };
        log.record("process_started")?;
        Ok((log, path))
    }

    pub fn writer(&self) -> Arc<File> {
        self.file.clone()
    }

    pub fn shutdown_requested(&self) -> io::Result<()> {
        self.record("shutdown_requested")
    }

    fn record(&self, event: &str) -> io::Result<()> {
        writeln!(
            &*self.file,
            "{event} time_unix_ms={} pid={} version={}",
            now_ms(),
            std::process::id(),
            env!("CARGO_PKG_VERSION")
        )?;
        self.file.sync_all()
    }

    // Explicitly called only after the event loop returns, never from Drop during unwind.
    // Some platforms exit inside their event loop; shutdown_requested is recorded there.
    pub fn finish(self) -> io::Result<()> {
        self.record("event_loop_returned")
    }
}

fn install_panic_hook(path: std::path::PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // A separate file avoids depending on tracing or its locks during a panic.
        let thread = std::thread::current();
        let report = format!(
            "time_unix_ms={} pid={} thread={:?}\n{info}\n{}\n",
            now_ms(),
            std::process::id(),
            thread.name(),
            Backtrace::force_capture()
        );
        if let Err(error) = write_panic(&path, &report) {
            eprintln!("Threadlane could not persist panic report: {error}");
        }
        previous(info);
    }));
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn write_panic(path: &Path, report: &str) -> io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(report.as_bytes())?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::{install_panic_hook, ProcessLog};
    use std::{fs, io::Write};

    #[test]
    fn persists_lifecycle_and_log_output() {
        let dir = tempfile::tempdir().unwrap();
        let (log, path) = ProcessLog::create(dir.path()).unwrap();
        writeln!(&*log.writer(), "session journal write failed: disk full").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("process_started"));
        assert!(!text.contains("event_loop_returned"));
        log.shutdown_requested().unwrap();
        log.finish().unwrap();
        let text = fs::read_to_string(path).unwrap();
        assert!(text.contains("session journal write failed: disk full"));
        assert!(text.contains("event_loop_returned"));
        assert!(text.contains("shutdown_requested"));
    }

    #[test]
    fn panic_hook_persists_report_before_process_exit() {
        const CHILD_DIR: &str = "THREADLANE_DIAGNOSTICS_TEST_CHILD_DIR";
        if let Some(dir) = std::env::var_os(CHILD_DIR) {
            let (_log, path) = ProcessLog::create(std::path::Path::new(&dir)).unwrap();
            install_panic_hook(path.with_extension("panic.log"));
            panic!("diagnostic panic probe");
        }
        let dir = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "diagnostics::tests::panic_hook_persists_report_before_process_exit",
                "--nocapture",
            ])
            .env(CHILD_DIR, dir.path())
            .output()
            .unwrap();
        assert!(!output.status.success());
        let paths = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        let report = paths
            .iter()
            .find(|path| path.to_string_lossy().ends_with(".panic.log"))
            .unwrap();
        let text = fs::read_to_string(report).unwrap();
        assert!(text.contains("diagnostic panic probe"));
        assert!(text.contains("diagnostics.rs:"));
        assert!(text.contains("pid="));
        assert!(text.contains("panic_hook_persists_report_before_process_exit"));
        let log = paths.iter().find(|path| *path != report).unwrap();
        let text = fs::read_to_string(log).unwrap();
        assert!(text.contains("process_started"));
        assert!(!text.contains("shutdown_requested"));
        assert!(!text.contains("event_loop_returned"));
    }
}
