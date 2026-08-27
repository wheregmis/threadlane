use std::fs;
use std::path::{Path, PathBuf};

const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_MATCHES: usize = 1_000;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

pub fn grep_search(root: &Path, pattern: &str, glob: Option<&str>) -> Result<String, String> {
    if pattern.is_empty() {
        return Err("search pattern must not be empty".into());
    }
    let mut files = Vec::new();
    collect_files(root, root, glob, &mut files)?;
    files.sort();
    let mut output = Vec::new();
    let mut output_bytes: usize = 0;
    let mut truncated = false;
    'files: for path in files {
        if fs::metadata(&path).is_ok_and(|metadata| metadata.len() > MAX_FILE_BYTES as u64) {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        if bytes.contains(&0) {
            continue;
        }
        let Ok(content) = std::str::from_utf8(&bytes) else {
            continue;
        };
        for (index, line) in content.lines().enumerate() {
            if line.contains(pattern) {
                let relative = path.strip_prefix(root).unwrap_or(&path).display();
                let formatted = format!("{}:{}:{}", relative, index + 1, line);
                let added_bytes = formatted.len() + usize::from(!output.is_empty());
                if output.len() == MAX_MATCHES
                    || output_bytes.saturating_add(added_bytes) > MAX_OUTPUT_BYTES
                {
                    truncated = true;
                    break 'files;
                }
                output_bytes += added_bytes;
                output.push(formatted);
            }
        }
    }
    if truncated {
        output.push("Search results truncated.".into());
    }
    Ok(if output.is_empty() {
        "No matches found.".into()
    } else {
        output.join("\n")
    })
}

fn collect_files(
    root: &Path,
    current: &Path,
    glob: Option<&str>,
    out: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let entries =
        fs::read_dir(current).map_err(|e| format!("failed to read {}: {e}", current.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let name = entry.file_name();
        if name == ".git" || name == "target" || name == ".threadlane" {
            continue;
        }
        if path.is_dir() {
            collect_files(root, &path, glob, out)?;
        } else if glob
            .map(|pattern| {
                simple_glob(
                    pattern,
                    path.strip_prefix(root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .as_ref(),
                )
            })
            .unwrap_or(true)
        {
            out.push(path);
        }
    }
    Ok(())
}

fn simple_glob(pattern: &str, value: &str) -> bool {
    match pattern.strip_prefix("**/") {
        Some(suffix) => value.ends_with(suffix),
        None if pattern.starts_with("*.") => value.ends_with(&pattern[1..]),
        None => value == pattern,
    }
}

#[cfg(test)]
mod tests {
    use super::{grep_search, MAX_FILE_BYTES, MAX_MATCHES};
    use std::fs;
    use std::process::Command;
    use std::time::{Duration, Instant};

    #[derive(Debug)]
    struct SearchBenchmark {
        in_process: Duration,
        rg: Option<Duration>,
        in_process_matches: usize,
        rg_matches: Option<usize>,
    }

    fn warm_run_benchmark(root: &std::path::Path, pattern: &str) -> SearchBenchmark {
        // Warm filesystem and executable caches outside the measurements.
        let _ = grep_search(root, pattern, None).unwrap();
        let _ = Command::new("rg")
            .args(["--", pattern, "."])
            .current_dir(root)
            .output();
        let start = Instant::now();
        let in_process = grep_search(root, pattern, None).unwrap();
        let in_process_elapsed = start.elapsed();
        let rg_start = Instant::now();
        let rg = Command::new("rg")
            .args(["--", pattern, "."])
            .current_dir(root)
            .output();
        let rg_elapsed = rg_start.elapsed();
        let (rg, rg_matches) = match rg {
            Ok(output) if output.status.success() => {
                let text = String::from_utf8_lossy(&output.stdout);
                (Some(rg_elapsed), Some(text.lines().count()))
            }
            _ => (None, None),
        };
        SearchBenchmark {
            in_process: in_process_elapsed,
            rg,
            in_process_matches: in_process
                .lines()
                .filter(|line| *line != "No matches found.")
                .count(),
            rg_matches,
        }
    }

    #[test]
    fn searches_without_shelling_out() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("one.rs"), "needle\nother\n").unwrap();
        fs::write(dir.path().join("two.txt"), "needle\n").unwrap();
        let result = grep_search(dir.path(), "needle", Some("*.rs")).unwrap();
        assert_eq!(result, "one.rs:1:needle");
    }

    #[test]
    fn grep_skips_binary_and_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("binary.bin"), b"needle\0hidden").unwrap();
        let mut large = b"needle\n".to_vec();
        large.resize(MAX_FILE_BYTES + 1, b'x');
        fs::write(dir.path().join("large.txt"), large).unwrap();
        fs::write(dir.path().join("small.txt"), b"needle\n").unwrap();

        assert_eq!(
            grep_search(dir.path(), "needle", None).unwrap(),
            "small.txt:1:needle"
        );
    }

    #[test]
    fn grep_caps_match_count() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("many.txt"),
            "needle\n".repeat(MAX_MATCHES + 1),
        )
        .unwrap();

        let result = grep_search(dir.path(), "needle", None).unwrap();

        assert_eq!(
            result
                .lines()
                .filter(|line| line.contains(":needle"))
                .count(),
            MAX_MATCHES
        );
        assert!(result.lines().last().unwrap().contains("truncated"));
    }

    #[test]
    #[ignore = "measurement harness; run with -- --ignored --nocapture"]
    fn warm_run_benchmark_against_rg() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..200 {
            fs::write(
                dir.path().join(format!("file-{index}.txt")),
                format!("line\nneedle {index}\nline\n"),
            )
            .unwrap();
        }
        let result = warm_run_benchmark(dir.path(), "needle");
        println!(
            "in_process={:?} matches={}; rg={:?} matches={:?}",
            result.in_process, result.in_process_matches, result.rg, result.rg_matches
        );
        if let Some(rg_matches) = result.rg_matches {
            assert_eq!(result.in_process_matches, rg_matches);
        }
    }
}
