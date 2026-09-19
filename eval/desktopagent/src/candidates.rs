use std::collections::HashSet;

use crate::model::{ActionKind, Candidate, Element, Observation};

#[derive(Debug, Clone, Default)]
pub struct CandidateContext {
    pub previously_clicked_editable: bool,
    pub previous_kind: Option<ActionKind>,
    pub previous_fingerprint: Option<String>,
    pub repeated_actions: HashSet<String>,
}

pub fn extract_literals(goal: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut quote = None;
    let mut start = 0;
    for (index, ch) in goal.char_indices() {
        match (quote, ch) {
            (None, '"' | '\'') => {
                quote = Some(ch);
                start = index + ch.len_utf8();
            }
            (Some(open), ch) if open == ch => {
                let value = goal[start..index].to_string();
                if !value.is_empty() && !out.contains(&value) {
                    out.push(value);
                }
                quote = None;
            }
            _ => {}
        }
    }
    out
}

pub fn generate(
    goal: &str,
    observation: &Observation,
    context: &CandidateContext,
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    let destructive = [
        "delete",
        "erase",
        "remove",
        "purchase",
        "pay",
        "send",
        "submit",
        "confirm purchase",
    ];
    let mut add = |mut candidate: Candidate| {
        let key = format!(
            "{}:{}",
            candidate.kind.label(),
            candidate
                .target
                .clone()
                .or(candidate.literal.clone())
                .unwrap_or_default()
        );
        if !context.repeated_actions.contains(&key) || candidate.terminal {
            candidate.id = format!("a{}", candidates.len());
            candidates.push(candidate);
        }
    };

    for element in &observation.elements {
        let label = element.label();
        if label.trim().is_empty()
            || destructive
                .iter()
                .any(|word| label.to_ascii_lowercase().contains(word))
        {
            continue;
        }
        let is_ocr = element
            .source
            .as_deref()
            .map(|source| source.to_ascii_lowercase().contains("ocr"))
            .unwrap_or(false);
        if element.is_clickable() || (is_ocr && element.bbox.is_some()) {
            let mut candidate = Candidate::action(
                ActionKind::Click,
                format!(
                    "Click the {} labelled {:?}.",
                    role_for_description(element),
                    label
                ),
                format!(
                    "Choose this when activating {:?} is the next step toward the user's goal.",
                    label
                ),
            );
            candidate.target = Some(element.id.clone());
            add(candidate);
        }
    }

    for menu in observation
        .menus
        .iter()
        .filter(|menu| menu.enabled && menu.action_supported)
    {
        if destructive
            .iter()
            .any(|word| menu.title.to_ascii_lowercase().contains(word))
        {
            continue;
        }
        let mut candidate = Candidate::action(
            ActionKind::Menu,
            format!("Select menu item {:?}.", menu.path),
            format!(
                "Choose this when selecting {:?} advances the user's goal.",
                menu.path
            ),
        );
        candidate.target = Some(menu.id.clone());
        add(candidate);
    }

    let scroll_target = observation
        .elements
        .iter()
        .find(|element| element.scrollable)
        .map(|element| element.id.clone());
    if let Some(target) = scroll_target {
        let mut scroll_up = Candidate::action(
            ActionKind::ScrollUp,
            "Scroll the current content upward.".into(),
            "Choose this when content above the current viewport is needed.".into(),
        );
        scroll_up.target = Some(target.clone());
        add(scroll_up);
        let mut scroll_down = Candidate::action(
            ActionKind::ScrollDown,
            "Scroll the current content downward.".into(),
            "Choose this when content below the current viewport is needed.".into(),
        );
        scroll_down.target = Some(target);
        add(scroll_down);
    }

    let focused = observation
        .focused_element_id
        .as_deref()
        .and_then(|id| observation.elements.iter().find(|element| element.id == id));
    let editable_focus = focused.map(Element::is_editable).unwrap_or(false);
    if editable_focus || context.previously_clicked_editable {
        for literal in extract_literals(goal) {
            let mut candidate = Candidate::action(
                ActionKind::Type,
                format!(
                    "Type the exact text {:?} into the focused editable field.",
                    literal
                ),
                format!(
                    "Choose this when entering {:?} is the next step required by the user's goal.",
                    literal
                ),
            );
            candidate.literal = Some(literal);
            add(candidate);
        }
    }

    if observation.focused_element_id.is_some() || context.previously_clicked_editable {
        add(Candidate::action(
            ActionKind::PressEnter,
            "Press Enter in the focused control.".into(),
            "Choose this when confirming or submitting the focused control advances the goal."
                .into(),
        ));
        add(Candidate::action(
            ActionKind::PressTab,
            "Press Tab to move to the next control.".into(),
            "Choose this when keyboard navigation to the next control advances the goal.".into(),
        ));
        add(Candidate::action(
            ActionKind::PressEscape,
            "Press Escape to dismiss the current transient UI.".into(),
            "Choose this when closing a dialog or transient menu advances the goal.".into(),
        ));
    }

    add(Candidate::terminal(
        crate::model::TerminalStatus::Done,
        "Choose this only when the user's requested goal is visibly complete.",
    ));
    add(Candidate::terminal(
        crate::model::TerminalStatus::Blocked,
        "Choose this when no available UI action can reasonably advance the goal.",
    ));
    candidates
}

fn role_for_description(element: &Element) -> &'static str {
    let role = element.normalized_role();
    if role.contains("button") {
        "button"
    } else if role.contains("link") {
        "link"
    } else if role.contains("field") || role.contains("search") {
        "field"
    } else {
        "control"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Element, Observation};

    fn element(id: &str, role: &str, text: &str) -> Element {
        Element {
            id: id.into(),
            role: role.into(),
            text: Some(text.into()),
            ..Default::default()
        }
    }

    #[test]
    fn extracts_only_explicit_literals() {
        assert_eq!(
            extract_literals(r#"Search for "invoice" and open it"#),
            vec!["invoice"]
        );
        assert!(extract_literals("search for invoices").is_empty());
    }

    #[test]
    fn editable_focus_allows_type() {
        let mut observation = Observation::default();
        observation.focused_element_id = Some("search".into());
        observation
            .elements
            .push(element("search", "AXSearchField", "Search"));
        let candidates = generate(r#"Search for "invoice""#, &observation, &Default::default());
        assert!(candidates
            .iter()
            .any(|candidate| candidate.kind == ActionKind::Type
                && candidate.literal.as_deref() == Some("invoice")));
    }

    #[test]
    fn non_editable_focus_does_not_allow_type() {
        let mut observation = Observation::default();
        observation.focused_element_id = Some("button".into());
        observation
            .elements
            .push(element("button", "AXButton", "Search"));
        let candidates = generate(r#"Search for "invoice""#, &observation, &Default::default());
        assert!(!candidates
            .iter()
            .any(|candidate| candidate.kind == ActionKind::Type));
    }

    #[test]
    fn static_text_is_not_clickable() {
        let mut observation = Observation::default();
        observation
            .elements
            .push(element("text", "AXStaticText", "hello"));
        let candidates = generate("click hello", &observation, &Default::default());
        assert!(!candidates
            .iter()
            .any(|candidate| candidate.kind == ActionKind::Click));
    }

    #[test]
    fn ax_button_is_clickable() {
        let mut observation = Observation::default();
        observation
            .elements
            .push(element("reply", "AXButton", "Reply"));
        let candidates = generate("click Reply", &observation, &Default::default());
        assert!(candidates.iter().any(|candidate| {
            candidate.kind == ActionKind::Click && candidate.target.as_deref() == Some("reply")
        }));
    }

    #[test]
    fn terminal_candidates_survive_suppression() {
        let context = CandidateContext {
            repeated_actions: ["done:".into(), "blocked:".into()].into_iter().collect(),
            ..Default::default()
        };
        let candidates = generate("done", &Observation::default(), &context);
        assert!(candidates.iter().any(|candidate| candidate.terminal
            && candidate.status == Some(crate::model::TerminalStatus::Done)));
        assert!(candidates.iter().any(|candidate| candidate.terminal
            && candidate.status == Some(crate::model::TerminalStatus::Blocked)));
    }

    #[test]
    fn scroll_candidates_target_the_scrollable_element() {
        let mut observation = Observation::default();
        observation.elements.push(Element {
            id: "scroll-area".into(),
            scrollable: true,
            ..Default::default()
        });
        let candidates = generate("scroll down", &observation, &Default::default());
        assert!(candidates.iter().any(|candidate| {
            candidate.kind == ActionKind::ScrollDown
                && candidate.target.as_deref() == Some("scroll-area")
        }));
    }
}
