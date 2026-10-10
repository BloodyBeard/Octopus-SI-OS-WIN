use super::*;
use insta::assert_snapshot;
use octopus_world_terminal_client::AccountUsageSummary;
use pretty_assertions::assert_eq;

fn bucket(date: &str, tokens: i64) -> AccountUsageBucket {
    let seconds = NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .expect("valid date")
        .and_hms_opt(0, 0, 0)
        .expect("valid time")
        .and_utc()
        .timestamp();
    AccountUsageBucket {
        bucket_start_seconds: seconds,
        total_tokens: tokens,
        ..Default::default()
    }
}

#[test]
fn duplicate_dates_sum_and_negative_values_clamp() {
    let today = NaiveDate::from_ymd_opt(2026, 5, 29).expect("valid date");
    let buckets = vec![
        bucket("2026-05-29", 10),
        bucket("2026-05-29", 5),
        bucket("2026-05-28", -4),
    ];

    let values = daily_values(&buckets, today);

    assert_eq!(values.iter().sum::<i64>(), 15);
}

#[test]
fn bar_levels_fill_from_bottom() {
    let levels = bar_levels(&[0, 10]);

    assert_eq!(&levels[..DAY_COUNT], &[0; DAY_COUNT]);
    assert_eq!(&levels[DAY_COUNT..], &[4; DAY_COUNT]);
}

#[test]
fn token_activity_view_aliases_parse() {
    assert_eq!(TokenActivityView::parse(""), Some(TokenActivityView::Daily));
    assert_eq!(
        TokenActivityView::parse("day"),
        Some(TokenActivityView::Daily)
    );
    assert_eq!(
        TokenActivityView::parse("week"),
        Some(TokenActivityView::Weekly)
    );
    assert_eq!(
        TokenActivityView::parse("cumulative"),
        Some(TokenActivityView::Cumulative)
    );
    assert_eq!(TokenActivityView::parse("year"), None);
}

#[test]
fn daily_graph_snapshot_uses_distinct_empty_and_active_cells() {
    let today = NaiveDate::from_ymd_opt(2026, 5, 29).expect("valid date");
    let buckets = vec![bucket("2026-05-25", 1), bucket("2026-05-29", 4)];

    let rendered = chart_lines(TokenActivityView::Daily, &buckets, today, 22)
        .into_iter()
        .map(|line| line.to_string().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n");

    assert_snapshot!(rendered, @r"
         Apr     May
    Su □ □ □ □ □ □ □ □ □
    Mo □ □ □ □ □ □ □ □ ■
    Tu □ □ □ □ □ □ □ □ □
    We □ □ □ □ □ □ □ □ □
    Th □ □ □ □ □ □ □ □ □
    Fr □ □ □ □ □ □ □ □ ■
    Sa □ □ □ □ □ □ □ □

      Less □ ■ ■ ■ ■ More
      daily · weekly · cumulative
    ");
}

#[test]
fn daily_graph_snapshot_stays_left_aligned_in_wide_terminal() {
    assert_eq!(graph_width(160), 107);
    assert_eq!(graph_width(u16::MAX), u16::MAX);

    let today = NaiveDate::from_ymd_opt(2026, 5, 29).expect("valid date");
    let lines = chart_lines(TokenActivityView::Daily, &[], today, 160);
    let rendered = [&lines[0], &lines[1], lines.last().expect("legend line")]
        .into_iter()
        .map(|line| line.to_string().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n");

    assert_snapshot!(rendered, @"
        Jun       Jul     Aug       Sep     Oct     Nov       Dec     Jan     Feb     Mar       Apr     May
     Su □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □ □
       daily · weekly · cumulative
    ");
}

#[test]
fn weekly_graph_snapshot_renders_bar_chart_and_caption() {
    let today = NaiveDate::from_ymd_opt(2026, 5, 29).expect("valid date");
    let buckets = vec![
        bucket("2026-05-11", 3),
        bucket("2026-05-18", 6),
        bucket("2026-05-25", 9),
    ];

    let rendered = chart_lines(TokenActivityView::Weekly, &buckets, today, 22)
        .into_iter()
        .map(|line| line.to_string().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n");

    assert_snapshot!(rendered, @"
          Apr     May
    max                 █
                        █
                      █ █
                      █ █
                    █ █ █
                    █ █ █
      0             █ █ █

       Each column = 1 week · tallest 9
       daily · weekly · cumulative
    ");
}

#[test]
fn cumulative_graph_snapshot_renders_running_total_bar_chart_and_caption() {
    let today = NaiveDate::from_ymd_opt(2026, 5, 29).expect("valid date");
    let buckets = vec![
        bucket("2026-05-11", 3),
        bucket("2026-05-18", 6),
        bucket("2026-05-25", 9),
    ];

    let rendered = chart_lines(TokenActivityView::Cumulative, &buckets, today, 22)
        .into_iter()
        .map(|line| line.to_string().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n");

    assert_snapshot!(rendered, @"
          Apr     May
    max                 █
                        █
                        █
                      █ █
                      █ █
                    █ █ █
      0             █ █ █

       Running total · top 18
       daily · weekly · cumulative
    ");
}

#[test]
fn summary_snapshot_left_aligns_and_splits_when_needed() {
    let report = AccountUsageReport {
        daily: Vec::new(),
        summary: Some(AccountUsageSummary {
            lifetime_tokens: Some(21_400_000_000),
            peak_daily_tokens: Some(835_000_000),
            longest_running_turn_sec: Some(13_920),
            current_streak_days: Some(54),
            longest_streak_days: Some(54),
        }),
    };
    let rendered = |width| {
        summary_lines(&report, graph_width(width))
            .into_iter()
            .map(|line| line.to_string().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };

    assert_snapshot!(
        format!(
            "wide:\n{}\n\nnarrow:\n{}\n\ntight:\n{}",
            rendered( 120),
            rendered( 80),
            rendered( 62)
        ),
        @"
    wide:
     Lifetime 21.4B · Peak 835M · Streak 54d · Longest task 3h 52m

    narrow:
     Lifetime 21.4B · Peak 835M · Streak 54d · Longest task 3h 52m

    tight:
     Lifetime 21.4B · Peak 835M · Streak 54d
     Longest task 3h 52m
    "
    );
}
