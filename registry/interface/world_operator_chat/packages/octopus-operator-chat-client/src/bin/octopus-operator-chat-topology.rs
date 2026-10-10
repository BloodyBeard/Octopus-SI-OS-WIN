use std::io::Read;

use octopus_operator_chat_client::{
    plan_operator_chat_topology, OperatorChatPlanRequest,
};

fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if !arguments.is_empty() {
        eprintln!("operator chat topology accepts no arguments");
        std::process::exit(1);
    }
    let mut input = String::new();
    if let Err(error) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("operator chat topology stdin read failed: {error}");
        std::process::exit(1);
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
