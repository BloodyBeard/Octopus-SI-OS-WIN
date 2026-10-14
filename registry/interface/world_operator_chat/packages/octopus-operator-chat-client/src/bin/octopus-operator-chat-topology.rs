use std::io::Read;

use octopus_operator_chat_client::{
    plan_operator_chat_topology, plan_operator_writer_runs, OperatorChatPlanRequest,
    OperatorWriterRunPlanRequest,
    plan_operator_writer_schema, OperatorWriterSchemaPlanRequest,
};
use octopus_operator_chat_client::run_revision::{
    RunRevisionRequest, RunRevisionVerification, prepare_run_revision, verify_run_revision,
};
use octopus_operator_chat_client::native_run_authoring::{NativeRunAdoptionRequest, plan_existing_adoption};

fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
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
    let native_adoption = arguments.len() == 1 && arguments[0] == "--plan-native-adoption";
    if !arguments.is_empty() && !writers_only && !writer_schema && !prepare_revision && !verify_revision && !native_adoption && !retirement_preflight {
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
    if prepare_revision || verify_revision {
        let result = if prepare_revision {
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
