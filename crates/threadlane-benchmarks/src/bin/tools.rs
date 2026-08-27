use std::fs;
use std::path::Path;

use threadlane_tools::search::grep_search;

const SAMPLES: usize = 10;

#[hotpath::measure]
fn search_warm_tree(root: &Path) {
    for _ in 0..20 {
        std::hint::black_box(grep_search(root, "needle", Some("*.txt")).unwrap());
    }
}

#[hotpath::main]
fn main() {
    let directory = tempfile::tempdir().unwrap();
    for index in 0..200 {
        fs::write(
            directory.path().join(format!("file-{index:03}.txt")),
            "fixed text containing needle\n",
        )
        .unwrap();
    }

    grep_search(directory.path(), "needle", Some("*.txt")).unwrap();
    for _ in 0..SAMPLES {
        search_warm_tree(directory.path());
    }
}
