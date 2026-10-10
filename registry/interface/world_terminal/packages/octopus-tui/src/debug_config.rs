use crate::history_cell::PlainHistoryCell;
use crate::provider_config_runtime::DebugReportLine;
use crate::provider_config_runtime::DebugReportLineKind;
use crate::provider_config_runtime::ProviderConfigRuntime;
use crate::provider_config_runtime::SessionProxyAddrs;
use crate::session_state::SessionNetworkProxyRuntime;
use ratatui::style::Stylize;
use ratatui::text::Line;

pub(crate) fn new_debug_config_output(
    config_runtime: &dyn ProviderConfigRuntime,
    session_network_proxy: Option<&SessionNetworkProxyRuntime>,
) -> PlainHistoryCell {
    let report =
        config_runtime.debug_report(session_network_proxy.map(|proxy| SessionProxyAddrs {
            http_addr: proxy.http_addr.clone(),
            socks_addr: proxy.socks_addr.clone(),
        }));
    PlainHistoryCell::new(
        report
            .into_iter()
            .map(report_line_to_display_line)
            .collect(),
    )
}

fn report_line_to_display_line(line: DebugReportLine) -> Line<'static> {
    match line.kind {
        DebugReportLineKind::CommandEcho => line.text.magenta().into(),
        DebugReportLineKind::SectionHeader => line.text.bold().into(),
        DebugReportLineKind::Text => line.text.into(),
        DebugReportLineKind::Muted => line.text.dim().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::report_line_to_display_line;
    use crate::provider_config_runtime::DebugReportLine;
    use crate::provider_config_runtime::DebugReportLineKind;
    use ratatui::style::Modifier;
    use ratatui::style::Style;
    use ratatui::style::Stylize;
    use ratatui::text::Line;

    fn line(kind: DebugReportLineKind, text: &str) -> Line<'static> {
        report_line_to_display_line(DebugReportLine {
            kind,
            text: text.to_string(),
        })
    }

    #[test]
    fn report_line_classes_map_to_the_terminal_styles() {
        let echo = line(DebugReportLineKind::CommandEcho, "/debug-config");
        assert_eq!(echo.spans[0].style, Style::new().magenta());

        let header = line(DebugReportLineKind::SectionHeader, "Requirements:");
        assert!(header.spans[0].style.add_modifier.contains(Modifier::BOLD));

        let text = line(DebugReportLineKind::Text, "  - name: value");
        assert_eq!(text.spans[0].style, Style::new());

        let muted = line(DebugReportLineKind::Muted, "  <none>");
        assert!(muted.spans[0].style.add_modifier.contains(Modifier::DIM));
    }
}
