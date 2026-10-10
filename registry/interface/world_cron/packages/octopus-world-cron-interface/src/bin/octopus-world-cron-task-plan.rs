use std::io::Read;

use octopus_operator_chat_client::{plan_operator_chat_topology, OperatorChatPlanRequest};

fn main() {
    let mut input = String::new();
    if let Err(error) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("scheduled task plan input could not be read: {error}");
        std::process::exit(2);
    }
    let request: OperatorChatPlanRequest = match serde_json::from_str(&input) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("scheduled task plan request is malformed: {error}");
            std::process::exit(2);
        }
    };
    match plan_operator_chat_topology(&request) {
        Ok(plan) => match serde_json::to_writer(std::io::stdout(), &plan) {
            Ok(()) => {}
            Err(error) => {
                eprintln!("scheduled task plan could not be encoded: {error}");
                std::process::exit(3);
            }
        },
        Err(error) => {
            eprintln!("scheduled task plan failed: {error}");
            std::process::exit(1);
        }
    }
}
