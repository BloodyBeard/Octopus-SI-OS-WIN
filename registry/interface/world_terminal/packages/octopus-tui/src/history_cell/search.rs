#[cfg(test)]
use super::*;

#[cfg(test)]
fn web_search_header(completed: bool) -> &'static str {
    if completed {
        "Searched the web"
    } else {
        "Searching the web"
    }
}

#[cfg(test)]
fn web_search_action_detail(action: &WebSearchActionItem) -> String {
    match action {
        WebSearchActionItem::Search { query, queries } => {
            query.clone().filter(|q| !q.is_empty()).unwrap_or_else(|| {
                let first = queries.first().cloned().unwrap_or_default();
                if queries.len() > 1 && !first.is_empty() {
                    format!("{first} ...")
                } else {
                    first
                }
            })
        }
        WebSearchActionItem::OpenPage { url } => url.clone().unwrap_or_default(),
        WebSearchActionItem::FindInPage { url, pattern } => match (pattern, url) {
            (Some(pattern), Some(url)) => format!("'{pattern}' in {url}"),
            (Some(pattern), None) => format!("'{pattern}'"),
            (None, Some(url)) => url.clone(),
            (None, None) => String::new(),
        },
        WebSearchActionItem::Other => String::new(),
    }
}

#[cfg(test)]
fn web_search_detail(action: Option<&WebSearchActionItem>, query: &str) -> String {
    let detail = action.map(web_search_action_detail).unwrap_or_default();
    if detail.is_empty() {
        query.to_string()
    } else {
        detail
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct WebSearchCell {
    query: String,
    action: Option<WebSearchActionItem>,
    start_time: Instant,
    completed: bool,
    animations_enabled: bool,
}

#[cfg(test)]
impl WebSearchCell {
    pub(crate) fn new(
        query: String,
        action: Option<WebSearchActionItem>,
        animations_enabled: bool,
    ) -> Self {
        Self {
            query,
            action,
            start_time: Instant::now(),
            completed: false,
            animations_enabled,
        }
    }

    pub(crate) fn complete(&mut self) {
        self.completed = true;
    }
}

#[cfg(test)]
impl HistoryCell for WebSearchCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let bullet = if self.completed {
            "•".dim()
        } else {
            activity_indicator(
                Some(self.start_time),
                MotionMode::from_animations_enabled(self.animations_enabled),
                ReducedMotionIndicator::StaticBullet,
            )
            .unwrap_or_else(|| "•".dim())
        };
        let header = web_search_header(self.completed);
        let detail = web_search_detail(self.action.as_ref(), &self.query);
        let text: Text<'static> = if detail.is_empty() {
            Line::from(vec![header.bold()]).into()
        } else {
            let separator = if self.completed { " for " } else { " " };
            Line::from(vec![header.bold(), separator.into(), detail.into()]).into()
        };
        PrefixedWrappedHistoryCell::new(text, vec![bullet, " ".into()], "  ").display_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let header = web_search_header(self.completed);
        let detail = web_search_detail(self.action.as_ref(), &self.query);
        if detail.is_empty() {
            vec![Line::from(header)]
        } else {
            let separator = if self.completed { " for " } else { " " };
            vec![Line::from(format!("{header}{separator}{detail}"))]
        }
    }
}

#[cfg(test)]
pub(crate) fn new_web_search_call(
    query: String,
    action: WebSearchActionItem,
) -> WebSearchCell {
    let mut cell = WebSearchCell::new(query, Some(action), false);
    cell.complete();
    cell
}
