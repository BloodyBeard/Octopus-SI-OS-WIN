use std::io::Read;

#[path = "octopus-operator-chat-topology/publication.rs"]
mod file_publication;

use octopus_operator_chat_client::{
    plan_operator_chat_topology, plan_operator_writer_runs, OperatorChatPlanRequest,
    OperatorWriterRunPlanRequest,
    plan_operator_writer_schema, OperatorWriterSchemaPlanRequest,
};
use octopus_operator_chat_client::run_revision::{
    InputPayloadInspection, RunRevisionRequest, RunRevisionVerification,
    inspect_input_payloads, prepare_run_revision, verify_run_revision,
};
use octopus_operator_chat_client::native_run_authoring::{NativeRunAdoptionRequest, plan_existing_adoption};

fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() == 1 && arguments[0] == "--publish-files" {
        match read_publication_request().and_then(file_publication::publish_files) {
            Ok(result) => match serde_json::to_string(&result) {
                Ok(encoded) => println!("{encoded}"),
                Err(error) => {
                    eprintln!("file publication result did not encode: {error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("file publication request failed closed: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if arguments.len() == 1 && matches!(arguments[0].to_str(), Some("--snapshot-directory" | "--publish-directory-moves" | "--reclaim-directory")) {
        let result = directory_publication(arguments[0].to_str().unwrap())
            .and_then(|result| serde_json::to_string(&result).map_err(|error| error.to_string()));
        match result {
            Ok(encoded) if encoded.len() as u64 <= PUBLICATION_MAX_BYTES => println!("{encoded}"),
            Ok(_) => { eprintln!("directory publication response exceeds its byte bound"); std::process::exit(1); }
            Err(error) => { eprintln!("directory publication failed closed: {error}"); std::process::exit(1); }
        }
        return;
    }
    if arguments.len() == 1 && arguments[0] == "--hold-task-retirement" {
        if let Err(error) = octopus_operator_chat_client::task_retirement::serve_task_retirement() {
            eprintln!("operator task retirement failed closed: {error}");
            std::process::exit(1);
        }
        return;
    }
    let retirement_preflight = arguments.len() == 1 && arguments[0] == "--verify-task-retirement";
    let writers_only = arguments.len() == 1 && arguments[0] == "--writer-runs";
    let writer_schema = arguments.len() == 1 && arguments[0] == "--writer-schema";
    let prepare_revision = arguments.len() == 1 && arguments[0] == "--prepare-input-revision";
    let verify_revision = arguments.len() == 1 && arguments[0] == "--verify-input-revision";
    let inspect_payloads = arguments.len() == 1 && arguments[0] == "--inspect-input-payloads";
    let native_adoption = arguments.len() == 1 && arguments[0] == "--plan-native-adoption";
    if !arguments.is_empty() && !writers_only && !writer_schema && !prepare_revision && !verify_revision && !inspect_payloads && !native_adoption && !retirement_preflight {
        eprintln!("operator chat topology received an unknown planning operation");
        std::process::exit(1);
    }
    let mut input = String::new();
    if let Err(error) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("operator chat topology stdin read failed: {error}");
        std::process::exit(1);
    }
    if retirement_preflight {
        let result = serde_json::from_str::<octopus_operator_chat_client::task_retirement::TaskRetirementContext>(&input)
            .map_err(|error| error.to_string()).and_then(|context| context.verify_before());
        match result {
            Ok(()) => println!("{{\"ready\":true}}"),
            Err(error) => { eprintln!("operator task retirement preflight failed closed: {error}"); std::process::exit(1); }
        }
        return;
    }
    if native_adoption {
        match serde_json::from_str::<NativeRunAdoptionRequest>(&input).map_err(|error|error.to_string())
            .and_then(|request| plan_existing_adoption(&request))
            .and_then(|plan| serde_json::to_string(&plan).map_err(|error|error.to_string())) {
            Ok(encoded) => println!("{encoded}"),
            Err(error) => { eprintln!("operator native adoption failed closed: {error}"); std::process::exit(1); }
        }
        return;
    }
    if prepare_revision || verify_revision || inspect_payloads {
        let result = if inspect_payloads {
            serde_json::from_str::<InputPayloadInspection>(&input).map_err(|error| error.to_string()).and_then(inspect_input_payloads)
        } else if prepare_revision {
            serde_json::from_str::<RunRevisionRequest>(&input).map_err(|error| error.to_string()).and_then(prepare_run_revision)
        } else {
            serde_json::from_str::<RunRevisionVerification>(&input).map_err(|error| error.to_string()).and_then(verify_run_revision)
        };
        match result {
            Ok(value) => println!("{value}"),
            Err(error) => {
                eprintln!("operator input revision failed closed: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if writers_only || writer_schema {
        let result = if writer_schema {
            serde_json::from_str::<OperatorWriterSchemaPlanRequest>(&input)
                .map_err(|error| error.to_string()).and_then(|request| plan_operator_writer_schema(&request))
        } else { serde_json::from_str::<OperatorWriterRunPlanRequest>(&input)
            .map_err(|error| error.to_string())
            .and_then(|request| plan_operator_writer_runs(&request)) }
            .and_then(|writes| serde_json::to_string(&writes).map_err(|error| error.to_string()));
        match result {
            Ok(encoded) => println!("{encoded}"),
            Err(error) => {
                eprintln!("operator writer Run planning failed closed: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    let request: OperatorChatPlanRequest = match serde_json::from_str(&input) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("operator chat topology request is malformed: {error}");
            std::process::exit(1);
        }
    };
    match plan_operator_chat_topology(&request) {
        Ok(plan) => match serde_json::to_string(&plan) {
            Ok(encoded) => println!("{encoded}"),
            Err(error) => {
                eprintln!("operator chat topology plan did not encode: {error}");
                std::process::exit(1);
            }
        },
        Err(error) => {
            eprintln!("operator chat topology planning failed closed: {error}");
            std::process::exit(1);
        }
    }
}

const PUBLICATION_MAX_BYTES: u64 = 64 * 1024 * 1024;
const PUBLICATION_MAX_WRITES: usize = 1024;

fn read_publication_request<T: serde::de::DeserializeOwned>() -> Result<T, String> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take(PUBLICATION_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > PUBLICATION_MAX_BYTES {
        return Err("publication request exceeds its byte bound".into());
    }
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectorySnapshotRequest {
    path: std::path::PathBuf,
    digest_algorithm: octopus_file_storage::publication::DigestAlgorithm,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryMovesRequest {
    operation_id: String,
    moves: Vec<octopus_file_storage::publication::DirectoryMove>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryReclaimRequest {
    tombstone: std::path::PathBuf,
    publication: octopus_file_storage::publication::DirectoryMoveReceipt,
    expected: octopus_file_storage::publication::DirectoryPreimage,
}

fn directory_publication(mode: &str) -> Result<serde_json::Value, String> {
    use octopus_file_storage::publication::{self, DigestAlgorithm};

    match mode {
        "--snapshot-directory" => {
            let request: DirectorySnapshotRequest = read_publication_request()?;
            if request.digest_algorithm != DigestAlgorithm::Sha256 {
                return Err("client directory publication requires SHA-256".into());
            }
            let preimage = publication::snapshot_directory(&request.path, request.digest_algorithm)
                .map_err(|error| error.to_string())?;
            Ok(serde_json::json!({"path": request.path, "preimage": preimage}))
        }
        "--publish-directory-moves" => {
            let request: DirectoryMovesRequest = read_publication_request()?;
            if request
                .moves
                .iter()
                .any(|movement| movement.expected.digest_algorithm != DigestAlgorithm::Sha256)
            {
                return Err("client directory publication requires SHA-256".into());
            }
            let result =
                publication::publish_directory_moves(&request.operation_id, &request.moves)
                    .map_err(|error| error.to_string())?;
            serde_json::to_value(result).map_err(|error| error.to_string())
        }
        "--reclaim-directory" => {
            let request: DirectoryReclaimRequest = read_publication_request()?;
            if request.expected.digest_algorithm != DigestAlgorithm::Sha256 {
                return Err("client directory publication requires SHA-256".into());
            }
            publication::reclaim_directory(
                &request.tombstone,
                &request.publication,
                &request.expected,
            )
            .map_err(|error| error.to_string())?;
            Ok(
                serde_json::json!({"tombstone": request.tombstone, "publication": request.publication, "reclaimed": true}),
            )
        }
        _ => Err("unknown directory publication operation".into()),
    }
}
