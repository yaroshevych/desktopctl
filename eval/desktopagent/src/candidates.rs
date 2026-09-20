use std::collections::HashSet;

use crate::model::{ActionKind, Candidate, Element, Observation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExactMenuMatch {
    None,
    Ambiguous,
    Unique,
}

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

/// Normalize the small, user-visible part of a menu label used by the local
/// fast path. In particular, macOS often renders a trailing ellipsis as the
/// single Unicode `…` character while a goal uses three ASCII dots.
pub fn normalize_menu_text(text: &str) -> String {
    text.replace('…', "")
        .replace("...", "")
        .chars()
        .map(|character| {
            if character == '>' {
                '>'
            } else if character.is_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split('>')
        .map(|part| part.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join(" > ")
        .trim()
        .to_string()
}

fn explicit_menu_request(goal: &str) -> Option<String> {
    let goal = goal.trim().trim_end_matches(['.', '!', '?']);
    ["click ", "choose ", "select ", "open "]
        .iter()
        .find_map(|prefix| {
            goal.get(..prefix.len())
                .filter(|head| head.eq_ignore_ascii_case(prefix))
                .and_then(|_| goal.get(prefix.len()..))
                .map(str::trim)
        })
        .filter(|request| !request.is_empty())
        .map(normalize_menu_text)
        .filter(|request| !request.is_empty())
}

fn menu_is_safe(menu: &crate::model::MenuItem) -> bool {
    [
        "delete", "erase", "remove", "purchase", "pay", "send", "submit",
    ]
    .iter()
    .all(|word| !menu.title.to_ascii_lowercase().contains(word))
}

fn menu_candidate(menu: &crate::model::MenuItem) -> Candidate {
    let mut candidate = Candidate::action(
        ActionKind::Menu,
        format!("Select menu item {:?}.", menu.path),
        format!(
            "Select the available enabled menu command {:?}. Its title matches words explicitly requested in the user's goal. Choose this when invoking that named command advances the goal.",
            menu.path
        ),
    );
    candidate.id = "a0".into();
    candidate.target = Some(menu.id.clone());
    candidate
}

/// Return a candidate only when an explicit menu request has one safe exact
/// match. Ambiguous labels deliberately never guess.
pub fn exact_menu_match(
    goal: &str,
    observation: &Observation,
) -> (ExactMenuMatch, Option<Candidate>) {
    let Some(request) = explicit_menu_request(goal) else {
        return (ExactMenuMatch::None, None);
    };
    let matches = observation
        .menus
        .iter()
        .filter(|menu| menu.enabled && menu.action_supported && menu_is_safe(menu))
        .filter(|menu| {
            let title = normalize_menu_text(&menu.title);
            let path = normalize_menu_text(&menu.path);
            let leaf = path.rsplit(" > ").next().unwrap_or(&path);
            request == title || request == path || request == leaf
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [menu] => (ExactMenuMatch::Unique, Some(menu_candidate(menu))),
        [] => (ExactMenuMatch::None, None),
        _ => (ExactMenuMatch::Ambiguous, None),
    }
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
    let goal_terms = semantic_terms(goal);
    let explicit_click_terms = goal
        .trim()
        .strip_prefix("Click ")
        .or_else(|| goal.trim().strip_prefix("click "))
        .map(semantic_terms)
        .filter(|terms| !terms.is_empty());
    let has_named_menu = observation.menus.iter().any(|menu| {
        menu.enabled
            && menu.action_supported
            && goal_terms
                .iter()
                .any(|term| semantic_terms(&menu.path).contains(term))
    });
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
        let label_terms = semantic_terms(&label);
        if explicit_click_terms
            .as_ref()
            .is_some_and(|requested| !label_terms.is_subset(requested))
        {
            continue;
        }
        if has_named_menu {
            if !goal_terms.iter().any(|term| label_terms.contains(term)) {
                continue;
            }
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

    let eligible_menus = observation
        .menus
        .iter()
        .filter(|menu| menu.enabled && menu.action_supported)
        .filter(|menu| {
            !destructive
                .iter()
                .any(|word| menu.title.to_ascii_lowercase().contains(word))
        })
        .collect::<Vec<_>>();
    let relevant_menus = eligible_menus
        .iter()
        .copied()
        .filter(|menu| {
            let menu_terms = semantic_terms(&menu.path);
            goal_terms.iter().any(|term| menu_terms.contains(term))
        })
        .collect::<Vec<_>>();
    let direct_menus = relevant_menus
        .iter()
        .copied()
        .filter(|menu| {
            let title_terms = semantic_terms(&menu.title);
            !title_terms.is_empty() && title_terms.iter().all(|term| goal_terms.contains(term))
        })
        .collect::<Vec<_>>();
    let menus = if !direct_menus.is_empty() {
        direct_menus
    } else if relevant_menus.is_empty() {
        eligible_menus.into_iter().take(40).collect::<Vec<_>>()
    } else {
        relevant_menus
    };
    for menu in menus {
        add(menu_candidate(menu));
    }

    let scroll_target = observation
        .elements
        .iter()
        .find(|element| element.scrollable)
        .map(|element| element.id.clone());
    if let Some(target) = scroll_target.filter(|_| !has_named_menu) {
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
        "Choose this when no available UI action can reasonably advance the goal. Do not choose blocked when an available candidate directly matches a control or command explicitly named by the user.",
    ));
    candidates
}

fn semantic_terms(text: &str) -> HashSet<String> {
    const STOP_WORDS: &[&str] = &[
        "and", "click", "choose", "first", "into", "latest", "newest", "open", "press", "select",
        "the", "then", "this", "type",
    ];
    text.split(|character: char| !character.is_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|term| term.len() >= 3 && !STOP_WORDS.contains(&term.as_str()))
        .collect()
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

    #[test]
    fn explicit_goal_prunes_unrelated_menus() {
        let mut observation = Observation::default();
        observation.menus = vec![
            crate::model::MenuItem {
                id: "settings".into(),
                path: "cmux > Settings".into(),
                title: "Settings".into(),
                enabled: true,
                action_supported: true,
            },
            crate::model::MenuItem {
                id: "print".into(),
                path: "File > Print".into(),
                title: "Print".into(),
                enabled: true,
                action_supported: true,
            },
        ];
        let candidates = generate("Click Settings", &observation, &Default::default());
        assert!(candidates.iter().any(|candidate| {
            candidate.kind == ActionKind::Menu && candidate.target.as_deref() == Some("settings")
        }));
        assert!(!candidates
            .iter()
            .any(|candidate| candidate.target.as_deref() == Some("print")));
    }

    #[test]
    fn explicit_click_never_falls_back_to_unrelated_path_button() {
        let mut observation = Observation::default();
        observation.elements = vec![
            element("settings", "AXButton", "Settings"),
            element("path", "AXButton", "/Users/oleg/Projects/settings-worktree"),
            element("reply", "AXButton", "Reply"),
        ];
        let candidates = generate("Click Settings", &observation, &Default::default());
        assert!(candidates
            .iter()
            .any(|candidate| candidate.target.as_deref() == Some("settings")));
        assert!(!candidates
            .iter()
            .any(|candidate| candidate.target.as_deref() == Some("path")));
        assert!(!candidates
            .iter()
            .any(|candidate| candidate.target.as_deref() == Some("reply")));
    }

    fn menu(id: &str, path: &str, title: &str) -> crate::model::MenuItem {
        crate::model::MenuItem {
            id: id.into(),
            path: path.into(),
            title: title.into(),
            enabled: true,
            action_supported: true,
        }
    }

    #[test]
    fn normalizes_menu_ellipsis_and_paths() {
        assert_eq!(normalize_menu_text("Settings…"), "settings");
        assert_eq!(normalize_menu_text("File > Settings..."), "file > settings");
    }

    #[test]
    fn exact_menu_match_requires_a_unique_safe_action() {
        let mut observation = Observation::default();
        observation.menus = vec![menu("settings", "File > Settings", "Settings…")];
        let (status, candidate) = exact_menu_match("Click Settings", &observation);
        assert_eq!(status, ExactMenuMatch::Unique);
        assert_eq!(
            candidate.and_then(|item| item.target),
            Some("settings".into())
        );
    }

    #[test]
    fn exact_menu_match_rejects_ambiguous_labels() {
        let mut observation = Observation::default();
        observation.menus = vec![
            menu("one", "File > Settings", "Settings"),
            menu("two", "View > Settings", "Settings…"),
        ];
        let (status, candidate) = exact_menu_match("Click Settings", &observation);
        assert_eq!(status, ExactMenuMatch::Ambiguous);
        assert!(candidate.is_none());
    }
}
