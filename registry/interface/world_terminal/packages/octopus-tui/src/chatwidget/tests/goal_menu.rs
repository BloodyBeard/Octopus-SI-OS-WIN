use super::*;

#[tokio::test]
async fn goal_menu_active_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let thread_id = uuid::Uuid::now_v7().to_string();

    chat.show_goal_summary(test_goal(thread_id, GoalState::Active, Some(80_000)));

    assert_chatwidget_snapshot!("goal_menu_active", rendered_goal_summary(&mut rx));
}

#[tokio::test]
async fn goal_menu_paused_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let thread_id = uuid::Uuid::now_v7().to_string();

    chat.show_goal_summary(test_goal(thread_id, GoalState::Paused, None));

    assert_chatwidget_snapshot!("goal_menu_paused", rendered_goal_summary(&mut rx));
}

#[tokio::test]
async fn goal_menu_blocked_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let thread_id = uuid::Uuid::now_v7().to_string();

    chat.show_goal_summary(test_goal(thread_id, GoalState::Blocked, None));

    assert_chatwidget_snapshot!("goal_menu_blocked", rendered_goal_summary(&mut rx));
}

#[tokio::test]
async fn goal_menu_usage_limited_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let thread_id = uuid::Uuid::now_v7().to_string();

    chat.show_goal_summary(test_goal(thread_id, GoalState::UsageLimited, None));

    assert_chatwidget_snapshot!("goal_menu_usage_limited", rendered_goal_summary(&mut rx));
}

#[tokio::test]
async fn goal_menu_budget_limited_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let thread_id = uuid::Uuid::now_v7().to_string();

    chat.show_goal_summary(test_goal(thread_id, GoalState::BudgetLimited, Some(80_000)));

    assert_chatwidget_snapshot!("goal_menu_budget_limited", rendered_goal_summary(&mut rx));
}

#[tokio::test]
async fn resume_paused_goal_prompt_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    let thread_id = uuid::Uuid::now_v7().to_string();

    chat.show_resume_paused_goal_prompt(
        thread_id,
        "Keep improving the bare goal command until it feels calm and useful.".to_string(),
    );

    assert_chatwidget_snapshot!("resume_paused_goal_prompt", render_bottom_popup(&chat, 100));
}

#[tokio::test]
async fn goal_edit_prompt_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(None).await;
    let thread_id = uuid::Uuid::now_v7().to_string();

    chat.show_goal_edit_prompt(
        thread_id.clone(),
        test_goal(thread_id.clone(), GoalState::Active, Some(80_000)),
    );

    assert_chatwidget_snapshot!("goal_edit_prompt", render_bottom_popup(&chat, 100));
}

#[tokio::test]
async fn goal_edit_prompt_submits_preserved_status_and_budget() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let thread_id = uuid::Uuid::now_v7().to_string();

    chat.show_goal_edit_prompt(
        thread_id.clone(),
        test_goal(thread_id.clone(), GoalState::Paused, Some(80_000)),
    );
    chat.handle_paste(" with clearer wording".to_string());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    match rx.try_recv() {
        Ok(AppEvent::SetThreadGoalDraft {
            thread_id: event_thread_id,
            draft,
            mode:
                crate::app_event::ThreadGoalSetMode::UpdateExisting {
                    status,
                    token_budget,
                },
        }) => {
            assert_eq!(event_thread_id, thread_id);
            assert_eq!(
                draft.objective,
                "Keep improving the bare goal command until it feels calm and useful. with clearer wording"
            );
            assert_eq!(status, GoalState::Paused);
            assert_eq!(token_budget, Some(80_000));
        }
        other => panic!("expected SetThreadGoalDraft event, got {other:?}"),
    }
    assert!(chat.no_modal_or_popup_active());
}

#[tokio::test]
async fn goal_edit_prompt_preserves_resumable_stopped_statuses() {
    for stopped_status in [GoalState::Blocked, GoalState::UsageLimited] {
        let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
        let thread_id = uuid::Uuid::now_v7().to_string();

        chat.show_goal_edit_prompt(
            thread_id.clone(),
            test_goal(thread_id.clone(), stopped_status, Some(80_000)),
        );
        chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

        match rx.try_recv() {
            Ok(AppEvent::SetThreadGoalDraft {
                mode:
                    crate::app_event::ThreadGoalSetMode::UpdateExisting {
                        status,
                        token_budget,
                    },
                ..
            }) => {
                assert_eq!(status, stopped_status);
                assert_eq!(token_budget, Some(80_000));
            }
            other => panic!("expected SetThreadGoalDraft event, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn goal_edit_prompt_resets_terminal_status_to_active() {
    let cases = [GoalState::BudgetLimited, GoalState::Complete];

    for terminal_status in cases {
        let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
        let thread_id = uuid::Uuid::now_v7().to_string();

        chat.show_goal_edit_prompt(
            thread_id.clone(),
            test_goal(thread_id.clone(), terminal_status, Some(80_000)),
        );
        chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

        match rx.try_recv() {
            Ok(AppEvent::SetThreadGoalDraft {
                mode:
                    crate::app_event::ThreadGoalSetMode::UpdateExisting {
                        status,
                        token_budget,
                    },
                ..
            }) => {
                assert_eq!(status, GoalState::Active);
                assert_eq!(token_budget, Some(80_000));
            }
            other => panic!("expected SetThreadGoalDraft event, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn resume_paused_goal_prompt_default_resumes_goal() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let thread_id = uuid::Uuid::now_v7().to_string();

    chat.show_resume_paused_goal_prompt(thread_id.clone(), "Finish the paused goal.".to_string());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    match rx.try_recv() {
        Ok(AppEvent::SetThreadGoalStatus {
            thread_id: event_thread_id,
            status,
        }) => {
            assert_eq!(event_thread_id, thread_id);
            assert_eq!(status, GoalState::Active);
        }
        other => panic!("expected SetThreadGoalStatus event, got {other:?}"),
    }
    assert!(chat.no_modal_or_popup_active());
}

#[tokio::test]
async fn resume_paused_goal_prompt_can_leave_goal_paused() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(None).await;
    let thread_id = uuid::Uuid::now_v7().to_string();

    chat.show_resume_paused_goal_prompt(thread_id, "Finish the paused goal.".to_string());
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    assert!(chat.no_modal_or_popup_active());
}

fn test_goal(_thread_id: String, status: GoalState, token_budget: Option<i64>) -> GoalStatusItem {
    GoalStatusItem {
        objective: "Keep improving the bare goal command until it feels calm and useful."
            .to_string(),
        status,
        token_budget,
        tokens_used: 12_500,
        time_used_seconds: 90,
    }
}

fn rendered_goal_summary(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::app_event::AppEvent>,
) -> String {
    drain_insert_history(rx)
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<Vec<_>>()
        .join("\n")
}
