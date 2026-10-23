use super::*;
use octopus_ledger::{GrantProjection, InvocationMetadata, encode_durable_movement_row};
use octopus_ledger_store::DurableMovementDecoder;

fn output_settlement_index(
    path: &Path,
    bytes: &[u8],
    owned: &BTreeSet<&InterfaceId>,
    max_row: u64,
) -> Result<(ArrivalDedupIndex, Vec<Movement>), String> {
    let view = AdoptionLedgerView::decode(path, bytes.to_vec(), max_row)?;
    let scopes = view.scopes(owned).into_iter().cloned().collect();
    Ok((view.index, scopes))
}

fn full_index(
    path: &Path,
    bytes: &[u8],
    owned: &BTreeSet<&InterfaceId>,
    max_row: u64,
) -> Result<(ArrivalDedupIndex, Vec<Movement>), String> {
    let decoder = DurableMovementDecoder::for_log(path);
    let mut index = ArrivalDedupIndex::new();
    let mut scopes = Vec::new();
    for (sequence, line) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        if !line.ends_with(b"\n") || line.len() as u64 > max_row {
            return Err("result adoption requires a complete bounded ledger prefix".into());
        }
        let row = decoder.decode(line, sequence).map_err(|e| e.to_string())?;
        if matches!(
            row.invocation_metadata,
            Some(MovementMetadata::ArrivalFanout(_))
        ) && matches!(&row.from, Endpoint::Interface(id) if owned.contains(id))
        {
            scopes.push(row.clone());
        }
        index
            .catch_up_movements(std::iter::once(row))
            .map_err(|e| e.to_string())?;
    }
    Ok((index, scopes))
}

fn receipt(index: &ArrivalDedupIndex, scopes: &[Movement]) -> Vec<Value> {
    scopes
        .iter()
        .map(|scope| match index.retained_arrival_edges(scope.seq.0) {
            Err(error) => json!({"scope":scope.seq.0,"error":error.to_string()}),
            Ok(edges) => {
                json!({"scope":scope.seq.0,"edges":edges.into_iter().map(|(coordinate, state)| {
                let state = match state {
                    RetainedArrivalState::Unseen => json!({"unseen":true}),
                    RetainedArrivalState::Pending(row) => json!({"pending":row.seq.0}),
                    RetainedArrivalState::Finished(verdict) => json!({"finished":verdict}),
                };
                json!({"coordinate":coordinate,"state":state})
            }).collect::<Vec<_>>()})
            }
        })
        .collect()
}

fn profile_index(path: &Path, bytes: &[u8]) -> (ArrivalDedupIndex, Vec<Movement>) {
    use std::time::{Duration, Instant};
    let decoder = DurableMovementDecoder::for_log(path);
    let mut index = ArrivalDedupIndex::new();
    let mut scopes = Vec::new();
    let mut projection = Duration::ZERO;
    let mut full = Duration::ZERO;
    let mut fold = Duration::ZERO;
    let mut full_rows = 0;
    for (sequence, line) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        assert!(line.ends_with(b"\n") && line.len() <= 1 << 20);
        let start = Instant::now();
        let (mut row, _) = decoder.decode_projection(line, sequence).unwrap();
        projection += start.elapsed();
        if requires_full_admission_grant(&row) {
            let start = Instant::now();
            row = decoder.decode(line, sequence).unwrap();
            full += start.elapsed();
            full_rows += 1;
        }
        let start = Instant::now();
        if matches!(
            row.invocation_metadata,
            Some(MovementMetadata::ArrivalFanout(_))
        ) {
            scopes.push(row.clone());
        }
        index.catch_up_movements(std::iter::once(row)).unwrap();
        fold += start.elapsed();
    }
    println!(
        "{}",
        json!({"projection_decode_ms":projection.as_secs_f64()*1000.0,
        "required_full_decode_ms":full.as_secs_f64()*1000.0,"index_fold_ms":fold.as_secs_f64()*1000.0,
        "required_full_rows":full_rows,"validated_grants":decoder.validated_grant_count().unwrap()})
    );
    (index, scopes)
}

fn encode(path: &Path, rows: &[Movement]) -> Vec<u8> {
    std::fs::create_dir_all(path.parent().unwrap().join("metadata")).unwrap();
    rows.iter()
        .enumerate()
        .map(|(line, row)| {
            let mut row = encode_durable_movement_row(row, line, |bytes| {
                let hash = ContentHash::of_bytes(bytes);
                std::fs::write(
                    path.parent().unwrap().join("metadata").join(hash.to_hex()),
                    bytes,
                )
                .unwrap();
                Ok(hash)
            })
            .unwrap();
            row.push('\n');
            row
        })
        .collect::<String>()
        .into_bytes()
}

#[test]
fn output_settlement_projection_preserves_verdicts_and_rejects_changed_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().canonicalize().unwrap().join("movements.jsonl");
    let provider: InterfaceId = "provider".into();
    let owned = BTreeSet::from([&provider]);
    let runtime = json!({"run":"run","ee":"ee","worker":"worker","checkpoint":"checkpoint",
        "interface":provider,"epoch":0,"run_record_hash":"a".repeat(64)});
    let artifact = json!({"id":"output","content":ContentHash::of_bytes(b"output")});
    let scope: Movement = serde_json::from_value(json!({
        "seq":0,"lamport":0,"recorded_at_unix_ms":1,"trace":"trace","kind":"ArtifactMoved",
        "from":{"Interface":provider},"to":{"Interface":provider},"artifact":artifact,
        "invocation_metadata":{"arrival_fanout":{"identity":"output","targets":[runtime]}}
    }))
    .unwrap();
    let mut invocation = InvocationMetadata::new(GrantProjection {
        tools: (0..1100).map(|i| format!("tool.{i}")).collect(),
        ..GrantProjection::default()
    })
    .with_runtime(serde_json::from_value(runtime.clone()).unwrap());
    invocation.delivery_scope_seq = Some(0);
    let mut arrival = scope.clone();
    arrival.seq.0 = 1;
    arrival.lamport.0 = 1;
    arrival.kind = MovementKind::InterfaceArrival;
    arrival.to = Endpoint::Interface("worker".into());
    arrival.invocation_metadata = Some(MovementMetadata::Invocation(invocation.clone()));
    let compare = |rows: &[Movement]| {
        let bytes = encode(&path, rows);
        let (before, scopes) = full_index(&path, &bytes, &owned, 1 << 20).unwrap();
        let (after, projected_scopes) =
            output_settlement_index(&path, &bytes, &owned, 1 << 20).unwrap();
        assert_eq!(scopes, projected_scopes);
        let expected = receipt(&before, &scopes);
        assert_eq!(expected, receipt(&after, &scopes));
        expected
    };
    assert_eq!(
        compare(&[scope.clone(), arrival.clone()])[0]["edges"][0]["state"],
        json!({"finished":{"admitted":{"seq":1}}})
    );
    let mut refused = arrival.clone();
    refused.kind = MovementKind::DeliveryRefused;
    refused.invocation_metadata = Some(
        serde_json::from_value(json!({
            "delivery_verdict":{"refused":{"reason":"policy","detail":{"rule":"denied"}}},
            "runtime":runtime,"delivery_scope_seq":0
        }))
        .unwrap(),
    );
    assert_eq!(
        compare(&[scope.clone(), refused])[0]["edges"][0]["state"]["finished"]["refused"]["reason"],
        "policy"
    );
    invocation.checkpoint_admission = Some(
        serde_json::from_value(json!({
            "artifact_label":"output","required_artifact_labels":["output"],"complete":true
        }))
        .unwrap(),
    );
    invocation.required_verified_artifacts = serde_json::from_value(json!([
        {"mark":"checked","artifact_label":"output","verifier":"verifier","params":{}}
    ]))
    .unwrap();
    arrival.invocation_metadata = Some(MovementMetadata::Invocation(invocation.clone()));
    assert_eq!(
        compare(&[scope.clone(), arrival.clone()])[0]["edges"][0]["state"],
        json!({"pending":1})
    );
    let mut replacement = arrival.clone();
    replacement.seq.0 = 2;
    replacement.lamport.0 = 2;
    invocation.replaces_arrival_seq = Some(1);
    invocation.runtime.as_mut().unwrap().run_record_hash = Some("b".repeat(64));
    replacement.invocation_metadata = Some(MovementMetadata::Invocation(invocation.clone()));
    let mut mark = replacement.clone();
    mark.seq.0 = 3;
    mark.lamport.0 = 3;
    mark.kind = MovementKind::MarkOutcomeAdmitted("checked".into());
    mark.invocation_metadata = Some(
        serde_json::from_value(json!({
            "name":"checked","subject":"checkpoint","run":"run","run_epoch":0,"class":"success",
            "evidence":artifact,"arrival_seq":2,"verifier":"verifier",
            "verifier_package":{"descriptor_content":ContentHash::of_bytes(b"descriptor"),
                "script_content":ContentHash::of_bytes(b"script")}
        }))
        .unwrap(),
    );
    assert_eq!(
        compare(&[
            scope.clone(),
            arrival.clone(),
            replacement.clone(),
            mark.clone()
        ])[0]["edges"][0]["state"],
        json!({"finished":{"admitted":{"seq":2}}})
    );
    invocation.grant.tools.push("changed".into());
    replacement.invocation_metadata = Some(MovementMetadata::Invocation(invocation));
    assert!(compare(&[scope.clone(), arrival.clone(), replacement, mark])[0]["error"].is_string());

    let bytes = encode(&path, &[scope, arrival]);
    for malformed in [bytes[..bytes.len() - 1].to_vec(), b"{}\n".to_vec()] {
        assert!(output_settlement_index(&path, &malformed, &owned, 1 << 20).is_err());
        assert!(full_index(&path, &malformed, &owned, 1 << 20).is_err());
    }
    assert!(output_settlement_index(&path, &bytes, &owned, 1).is_err());
    let mut rows: Vec<Value> = bytes
        .split(|b| *b == b'\n')
        .filter(|row| !row.is_empty())
        .map(|row| serde_json::from_slice(row).unwrap())
        .collect();
    let hash = rows[1]["invocation_metadata"]["grant_content"]
        .as_str()
        .unwrap()
        .to_owned();
    std::fs::write(path.parent().unwrap().join("metadata").join(hash), b"{}").unwrap();
    assert!(output_settlement_index(&path, &bytes, &owned, 1 << 20).is_err());
    assert!(full_index(&path, &bytes, &owned, 1 << 20).is_err());
    let malformed = br#"{"tools":false}"#;
    let hash = ContentHash::of_bytes(malformed).to_hex();
    std::fs::write(
        path.parent().unwrap().join("metadata").join(&hash),
        malformed,
    )
    .unwrap();
    rows[1]["invocation_metadata"]["grant_content"] = json!(hash);
    let bytes = rows
        .iter()
        .map(|row| format!("{row}\n"))
        .collect::<String>();
    assert!(output_settlement_index(&path, bytes.as_bytes(), &owned, 1 << 20).is_err());
    assert!(full_index(&path, bytes.as_bytes(), &owned, 1 << 20).is_err());
}

#[test]
#[ignore = "read-only benchmark; requires OCTOPUS_ADOPTION_BENCH_LEDGER"]
fn benchmark_output_settlement_fixed_prefix() {
    use std::io::Read;
    use std::time::Instant;
    let path = PathBuf::from(
        std::env::var_os("OCTOPUS_ADOPTION_BENCH_LEDGER").expect("explicit ledger path"),
    );
    let mut file = std::fs::File::open(&path).unwrap();
    let initial_length = file.metadata().unwrap().len();
    let prefix_length = std::env::var_os("OCTOPUS_ADOPTION_BENCH_PREFIX_BYTES")
        .map(|value| value.to_str().unwrap().parse::<u64>().unwrap())
        .unwrap_or(initial_length);
    assert!(prefix_length <= initial_length);
    let mut bytes = Vec::new();
    (&mut file)
        .take(prefix_length)
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes.len() as u64, prefix_length);
    let complete = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .expect("complete ledger row")
        + 1;
    bytes.truncate(complete);
    let prefix_hash = ContentHash::of_bytes(&bytes);
    let owned: BTreeSet<InterfaceId> = bytes
        .split_inclusive(|b| *b == b'\n')
        .enumerate()
        .map(|(line, bytes)| {
            let row = octopus_ledger::decode_durable_movement_route(bytes, line).unwrap();
            let Endpoint::Interface(id) = row.from;
            id
        })
        .collect();
    let owned = owned.iter().collect();
    let mut baseline = None;
    let modes = if std::env::var_os("OCTOPUS_ADOPTION_BENCH_PHASES").is_some() {
        vec!["profile"]
    } else {
        vec!["full", "projection", "projection", "full"]
    };
    for mode in modes {
        let start = Instant::now();
        let (index, scopes) = if mode == "full" {
            full_index(&path, &bytes, &owned, 1 << 20)
        } else if mode == "projection" {
            output_settlement_index(&path, &bytes, &owned, 1 << 20)
        } else {
            Ok(profile_index(&path, &bytes))
        }
        .unwrap();
        let scan = start.elapsed();
        let receipt = receipt(&index, &scopes);
        let digest = ContentHash::of_bytes(&serde_json::to_vec(&receipt).unwrap());
        let expected = baseline.get_or_insert(digest);
        assert_eq!(
            *expected, digest,
            "same prefix must retain every exact edge verdict"
        );
        println!(
            "{}",
            json!({"mode":mode,"prefix_bytes":bytes.len(),"prefix_hash":prefix_hash.to_hex(),
            "rows":index.watermark(),"scopes":scopes.len(),"scan_ms":scan.as_secs_f64()*1000.0,
            "verdict_ms":(start.elapsed()-scan).as_secs_f64()*1000.0,"verdict_hash":digest.to_hex()})
        );
    }
    let mut reread = Vec::new();
    std::fs::File::open(&path)
        .unwrap()
        .take(bytes.len() as u64)
        .read_to_end(&mut reread)
        .unwrap();
    assert_eq!(
        prefix_hash,
        ContentHash::of_bytes(&reread),
        "ledger prefix changed during measurement"
    );
}
