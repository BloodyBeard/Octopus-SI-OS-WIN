use std::io::Read;

use octopus_operator_chat_client::{
    plan_operator_chat_topology, plan_operator_writer_runs, OperatorChatPlanRequest,
    OperatorWriterRunPlanRequest,
    plan_operator_writer_schema, OperatorWriterSchemaPlanRequest,
};

fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let writers_only = arguments.len() == 1 && arguments[0] == "--writer-runs";
    let writer_schema = arguments.len() == 1 && arguments[0] == "--writer-schema";
    if !arguments.is_empty() && !writers_only && !writer_schema {
        eprintln!("operator chat topology accepts only --writer-runs or --writer-schema");
        std::process::exit(1);
    }
    let mut input = String::new();
    if let Err(error) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("operator chat topology stdin read failed: {error}");
        std::process::exit(1);
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
