use super::candidate::Candidate;
use super::candidate::MentionType;
use super::candidate::Selection;
use octopus_world_terminal_client::TaskMentionCandidate;

pub(crate) fn build_search_catalog(tasks: &[TaskMentionCandidate]) -> Vec<Candidate> {
    tasks
        .iter()
        .map(|task| {
            let mut mention = task
                .title
                .chars()
                .map(|character| {
                    if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                        character
                    } else {
                        '-'
                    }
                })
                .collect::<String>();
            mention = mention.trim_matches('-').to_string();
            if mention.is_empty() {
                mention = format!(
                    "task-{}",
                    &task.run.as_str()[..8.min(task.run.as_str().len())]
                );
            }
            Candidate {
                display_name: task.title.clone(),
                description: task.preview.clone(),
                search_terms: vec![
                    task.title.clone(),
                    task.run.as_str().to_string(),
                    task.preview.clone().unwrap_or_default(),
                ],
                mention_type: MentionType::Task,
                selection: Selection::Task {
                    mention,
                    uri: task.uri.clone(),
                },
            }
        })
        .collect()
}
