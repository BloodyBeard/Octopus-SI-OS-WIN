use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("ledger path");
    let bytes = std::fs::read(&path).unwrap();
    let decoder = octopus_ledger_store::DurableMovementDecoder::for_log(&path);
    let mut expected = Vec::new();
    let started = Instant::now();
    for (index, line) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        let m = decoder.decode(line, index).unwrap();
        expected.push((m.seq.0, m.from.clone(), m.to.clone(), m.runtime().map(|r| r.run.clone())));
    }
    let full_ms = started.elapsed().as_millis();
    let started = Instant::now();
    for (index, line) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        let (m, _) = decoder.decode_projection(line, index).unwrap();
        assert_eq!((m.seq.0, m.from.clone(), m.to.clone(), m.runtime().map(|r| r.run.clone())), expected[index]);
    }
    let cold_projection_ms = started.elapsed().as_millis();
    let started = Instant::now();
    for (index, line) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        let (m, _) = decoder.decode_projection(line, index).unwrap();
        assert_eq!((m.seq.0, m.from.clone(), m.to.clone(), m.runtime().map(|r| r.run.clone())), expected[index]);
    }
    println!("{}", serde_json::json!({"ledger_bytes":bytes.len(),"movements":expected.len(),"full_decode_ms":full_ms,
        "validated_projection_ms":cold_projection_ms,"warm_projection_ms":started.elapsed().as_millis(),
        "retirement_fields_equal":true}));
}
