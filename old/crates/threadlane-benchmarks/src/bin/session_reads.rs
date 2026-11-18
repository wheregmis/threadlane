//! Compare indexed branch reads with the reference SessionStore implementation.
//! Run with `cargo run --release -p threadlane-benchmarks --bin session_reads`.
use std::hint::black_box;
use std::io::Write;
use std::time::Instant;
use threadlane_protocol::AgentMessage;
use threadlane_runtime::harness::{
    Entry, JsonlStore, Record, ReduceError, SessionStore, SurfaceOperation,
};

struct ReferenceStore<'a>(&'a JsonlStore);

impl SessionStore for ReferenceStore<'_> {
    fn session_id(&self) -> &str {
        self.0.session_id()
    }
    fn entries(&self) -> &[Entry] {
        self.0.entries()
    }
    fn records(&self) -> &[Record] {
        self.0.records()
    }
    fn append_entry(&mut self, _: Entry) -> Result<(), ReduceError> {
        unreachable!()
    }
    fn append_record(&mut self, _: Record) -> Result<(), ReduceError> {
        unreachable!()
    }
}

fn sample(store: &impl SessionStore, leaf: &str, limit: usize, iterations: usize) -> f64 {
    let start = Instant::now();
    for _ in 0..iterations {
        black_box(store.branch(Some(black_box(leaf)), black_box(limit)));
    }
    start.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64
}

fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

fn main() {
    println!("entries,limit,reference_us,indexed_us,speedup");
    for count in [1_000usize, 10_000, 50_000] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("branch.jsonl");
        // Fixture creation and journal opening are outside the timed reads.
        let mut file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        for index in 0..count {
            let entry = Entry {
                id: format!("entry-{index}"),
                parent_id: index.checked_sub(1).map(|parent| format!("entry-{parent}")),
                lane: "main".into(),
                seq: index as u64 + 1,
                timestamp: 0,
                message: AgentMessage::user("x".repeat(256), vec![]),
                surface_op: SurfaceOperation::Append,
                terminate: false,
            };
            serde_json::to_writer(&mut file, &entry).unwrap();
            file.write_all(b"\n").unwrap();
        }
        file.flush().unwrap();
        let store = JsonlStore::open_read_only(&path).unwrap();
        let reference = ReferenceStore(&store);
        let leaf = format!("entry-{}", count - 1);
        for (limit, iterations) in [(32, 100), (count, 10)] {
            assert_eq!(
                store.branch(Some(&leaf), limit),
                reference.branch(Some(&leaf), limit)
            );
            sample(&reference, &leaf, limit, 3);
            sample(&store, &leaf, limit, 3);
            let mut old = Vec::new();
            let mut new = Vec::new();
            for round in 0..9 {
                // Alternate order to reduce systematic warm-cache/order bias.
                if round % 2 == 0 {
                    old.push(sample(&reference, &leaf, limit, iterations));
                    new.push(sample(&store, &leaf, limit, iterations));
                } else {
                    new.push(sample(&store, &leaf, limit, iterations));
                    old.push(sample(&reference, &leaf, limit, iterations));
                }
            }
            let old = median(old);
            let new = median(new);
            println!("{count},{limit},{old:.2},{new:.2},{:.2}", old / new);
        }
    }
}
