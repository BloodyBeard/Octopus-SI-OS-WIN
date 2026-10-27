use super::*;

fn test_run_target(run: &str, cwd: Option<&str>) -> crate::resume_picker::SessionTarget {
    crate::resume_picker::SessionTarget {
        run: octopus_descriptors::RunId::new(run),
        cwd: cwd.map(PathBuf::from),
    }
}

#[test]
fn startup_paused_goal_prompt_gate_is_only_for_quiet_resume() {
    let resume = SessionSelection::Resume(test_run_target("run_restore", Some("/tmp/restore")));
    let fork = SessionSelection::Fork(test_run_target("run_fork", Some("/tmp/fork")));
    let no_images: Vec<PathBuf> = Vec::new();
    let initial_images = vec![PathBuf::from("/tmp/image.png")];

    assert!(App::should_prompt_for_paused_goal_after_startup_resume(
        &resume, &None, &no_images
    ));
    assert!(!App::should_prompt_for_paused_goal_after_startup_resume(
        &resume,
        &Some("continue from here".to_string()),
        &no_images
    ));
    assert!(!App::should_prompt_for_paused_goal_after_startup_resume(
        &resume,
        &None,
        &initial_images
    ));
    assert!(!App::should_prompt_for_paused_goal_after_startup_resume(
        &SessionSelection::StartFresh,
        &None,
        &no_images
    ));
    assert!(!App::should_prompt_for_paused_goal_after_startup_resume(
        &fork, &None, &no_images
    ));
}
