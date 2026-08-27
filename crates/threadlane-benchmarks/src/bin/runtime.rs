use std::path::Path;
use threadlane_runtime::harness::{JsonlStore, MemoryStore, Record, Reducer, SessionStore};

const SAMPLES: usize = 10;

fn fact(id: String, seq: u64) -> Record {
    Record::FactSet {
        id,
        seq,
        lane: "main".into(),
        timestamp: 0,
        run_id: None,
        key: "bench".into(),
        value: "0123456789abcdef0123456789abcdef".into(),
    }
}

fn pad_to(store: &mut JsonlStore, n: usize) {
    while store.entries().len() + store.records().len() < n {
        let seq = store.next_sequence();
        store
            .append_record(fact(format!("fact-bench-{seq}"), seq))
            .unwrap();
    }
}

fn append_records(store: &mut JsonlStore) {
    for _ in 0..200 {
        let seq = store.next_sequence();
        store
            .append_record(fact(format!("fact-bench-{seq}"), seq))
            .unwrap();
    }
}

#[hotpath::measure]
fn append_scaling(store: &mut JsonlStore) {
    append_records(store);
}

#[hotpath::measure]
fn open_scaling(path: &Path) {
    for _ in 0..3 {
        std::hint::black_box(JsonlStore::open(path).unwrap());
    }
}

#[hotpath::measure]
fn reducer_replay(store: &MemoryStore) {
    std::hint::black_box(Reducer::reduce(store).unwrap());
}

#[hotpath::main]
fn main() {
    let append_template_dir = tempfile::tempdir().unwrap();
    let append_template_path = append_template_dir.path().join("append-template.jsonl");
    let mut append_template = JsonlStore::open(&append_template_path).unwrap();
    pad_to(&mut append_template, 4_000);
    drop(append_template);

    let warmup_dir = tempfile::tempdir().unwrap();
    let warmup_path = warmup_dir.path().join("append-warmup.jsonl");
    std::fs::copy(&append_template_path, &warmup_path).unwrap();
    let mut warmup_store = JsonlStore::open(warmup_path).unwrap();
    append_records(&mut warmup_store);

    let mut append_fixtures = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("append-scaling.jsonl");
        std::fs::copy(&append_template_path, &path).unwrap();
        append_fixtures.push((directory, JsonlStore::open(path).unwrap()));
    }

    let open_dir = tempfile::tempdir().unwrap();
    let open_path = open_dir.path().join("open-scaling.jsonl");
    let mut open_store = JsonlStore::open(&open_path).unwrap();
    pad_to(&mut open_store, 4_000);
    drop(open_store);

    let mut store = MemoryStore::new("reducer-replay");
    for seq in 1..=4_000 {
        store.append_record(fact(format!("fact-bench-{seq}"), seq));
    }

    std::hint::black_box(JsonlStore::open(&open_path).unwrap());
    std::hint::black_box(Reducer::reduce(&store).unwrap());

    for (_, append_store) in &mut append_fixtures {
        assert_eq!(append_store.records().len(), 4_000);
        append_scaling(append_store);
        open_scaling(&open_path);
        reducer_replay(&store);
    }
}
