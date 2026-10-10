use octopus_shell_command::parse_command::shlex_join;

pub fn resume_command(thread_name: Option<&str>, thread_id: Option<&str>) -> Option<String> {
    let resume_target = thread_name
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .or_else(|| thread_id.map(str::to_string));
    resume_target.map(|target| {
        let needs_double_dash = target.starts_with('-');
        let escaped = shlex_join(&[target]);
        if needs_double_dash {
            format!("octopus resume -- {escaped}")
        } else {
            format!("octopus resume {escaped}")
        }
    })
}

pub fn resume_hint(thread_name: Option<&str>, thread_id: Option<&str>) -> Option<String> {
    let thread_id = thread_id?;
    match thread_name.filter(|name| !name.is_empty()) {
        Some(thread_name) => Some(format!(
            "octopus resume, then select {thread_name} ({thread_id})"
        )),
        None => resume_command(None, Some(thread_id)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn prefers_name_over_id() {
        let thread_id = "123e4567-e89b-12d3-a456-426614174000";
        let command = resume_command(Some("my-thread"), Some(thread_id));
        assert_eq!(command, Some("octopus resume my-thread".to_string()));
    }

    #[test]
    fn formats_thread_id_when_name_is_missing() {
        let thread_id = "123e4567-e89b-12d3-a456-426614174000";
        let command = resume_command(None, Some(thread_id));
        assert_eq!(
            command,
            Some("octopus resume 123e4567-e89b-12d3-a456-426614174000".to_string())
        );
    }

    #[test]
    fn returns_none_without_a_resume_target() {
        let command = resume_command(None, None);
        assert_eq!(command, None);
    }

    #[test]
    fn quotes_thread_names_when_needed() {
        let command = resume_command(Some("-starts-with-dash"), None);
        assert_eq!(
            command,
            Some("octopus resume -- -starts-with-dash".to_string())
        );

        let command = resume_command(Some("two words"), None);
        assert_eq!(command, Some("octopus resume 'two words'".to_string()));

        let command = resume_command(Some("quote'case"), None);
        assert_eq!(command, Some("octopus resume \"quote'case\"".to_string()));
    }

    #[test]
    fn resume_hint_names_picker_item_with_id() {
        let thread_id = "123e4567-e89b-12d3-a456-426614174000";
        let hint = resume_hint(Some("my-thread"), Some(thread_id));
        assert_eq!(
            hint,
            Some(
                "octopus resume, then select my-thread (123e4567-e89b-12d3-a456-426614174000)"
                    .to_string()
            )
        );
    }

    #[test]
    fn resume_hint_uses_direct_id_command_without_name() {
        let thread_id = "123e4567-e89b-12d3-a456-426614174000";
        let hint = resume_hint(None, Some(thread_id));
        assert_eq!(
            hint,
            Some("octopus resume 123e4567-e89b-12d3-a456-426614174000".to_string())
        );
    }

    #[test]
    fn resume_hint_requires_thread_id() {
        let hint = resume_hint(Some("my-thread"), None);
        assert_eq!(hint, None);
    }
}
