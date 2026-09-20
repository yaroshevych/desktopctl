use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::candidates::{exact_menu_match, generate, CandidateContext, ExactMenuMatch};
use crate::desktopctl::{focused_element_id, DesktopCtl, DesktopCtlError};
use crate::jev::{JevClient, JevError};
use crate::model::{ActionKind, Candidate, Observation, TerminalStatus};
use serde_json::Value;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("{0}")]
    DesktopCtl(#[from] DesktopCtlError),
    #[error("{0}")]
    Jev(#[from] JevError),
    #[error("could not read context: {0}")]
    Context(String),
    #[error("invalid configuration: {0}")]
    Config(String),
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub active_window: Option<String>,
    pub context_path: Option<PathBuf>,
    pub workspace: Option<PathBuf>,
    pub max_steps: u32,
    pub confidence_threshold: f64,
    pub desktopctl_timeout: Duration,
    pub jev_timeout: Duration,
    pub run_timeout: Duration,
    pub trace: bool,
    pub trace_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunResult {
    pub status: String,
    pub message: String,
    pub steps: u32,
    pub elapsed_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub probabilities: BTreeMap<String, f64>,
}

impl RunResult {
    pub fn error(message: String) -> Self {
        Self {
            status: "error".into(),
            message,
            steps: 0,
            elapsed_ms: 0,
            reason: Some("error".into()),
            choice: None,
            confidence: None,
            probabilities: BTreeMap::new(),
        }
    }
}

pub struct Agent {
    config: AgentConfig,
    desktopctl: DesktopCtl,
    jev: JevClient,
    initial_context: Option<String>,
    run_id: String,
}

struct SpeculativeTiming {
    menu_ms: u128,
    tokenize_ms: Option<u128>,
    jev_ms: u128,
    path: &'static str,
}

struct SpeculativeDecision {
    observation: Observation,
    candidates: Vec<Candidate>,
    decision: crate::model::JevDecision,
    action_result: Option<Value>,
    timing: SpeculativeTiming,
}

impl Agent {
    pub fn new(config: AgentConfig) -> Result<Self, AgentError> {
        let mut config = config;
        if config.trace_file.is_none() && config.trace {
            if let Some(workspace) = &config.workspace {
                config.trace_file = Some(workspace.join("desktopagent.trace.jsonl"));
            }
        }
        if !(0.0..=1.0).contains(&config.confidence_threshold) {
            return Err(AgentError::Config(
                "confidence threshold must be between 0 and 1".into(),
            ));
        }
        let initial_context = config
            .context_path
            .as_ref()
            .map(|path| fs::read_to_string(path).map_err(|e| AgentError::Context(e.to_string())))
            .transpose()?;
        let desktopctl = DesktopCtl::new(config.desktopctl_timeout, config.active_window.clone())?;
        let jev = JevClient::new(config.jev_timeout)?;
        Ok(Self {
            config,
            desktopctl,
            jev,
            initial_context,
            run_id: Uuid::new_v4().to_string(),
        })
    }

    pub fn inspect(&self, goal: &str) -> Result<RunResult, AgentError> {
        let started = Instant::now();
        let observation = self.desktopctl.observe()?;
        let candidates = generate(goal, &observation, &CandidateContext::default());
        let decision = self.choose(goal, &observation, &candidates)?;
        let selected = find_candidate(&candidates, &decision.choice);
        self.trace_event(
            0,
            "inspect",
            started.elapsed().as_millis(),
            candidates.len(),
            Some(&decision.choice),
            decision.confidence,
            &decision.probabilities,
            None,
            selected,
        );
        Ok(RunResult {
            status: "inspect".into(),
            message: selected
                .map(|candidate| {
                    format!(
                        "Jev chose {}: {} (confidence {:.2}).",
                        decision.choice, candidate.description, decision.confidence
                    )
                })
                .unwrap_or_else(|| {
                    format!(
                        "Jev chose {} (confidence {:.2}).",
                        decision.choice, decision.confidence
                    )
                }),
            steps: 0,
            elapsed_ms: started.elapsed().as_millis(),
            reason: None,
            choice: Some(decision.choice),
            confidence: Some(decision.confidence),
            probabilities: decision.probabilities,
        })
    }

    pub fn step(&self, goal: &str) -> Result<RunResult, AgentError> {
        let started = Instant::now();
        let selected = self.speculative_decision(goal, &CandidateContext::default(), &[], 0)?;
        self.trace_speculative(0, &selected.timing);
        let candidates = selected.candidates;
        let decision = selected.decision;
        let action_result = selected.action_result;
        let selected = find_candidate(&candidates, &decision.choice);
        self.trace_event(
            0,
            "decision",
            started.elapsed().as_millis(),
            candidates.len(),
            Some(&decision.choice),
            decision.confidence,
            &decision.probabilities,
            None,
            selected,
        );
        if decision.confidence < self.config.confidence_threshold {
            return Ok(self.blocked_for_decision(0, started, "low_confidence", decision));
        }
        let candidate = find_candidate(&candidates, &decision.choice)
            .ok_or_else(|| AgentError::Config("Jev selected an unknown candidate".into()))?;
        if candidate.terminal {
            return Ok(self.terminal_result(candidate, 0, started));
        }
        if action_result.is_none() {
            self.desktopctl.execute(candidate)?;
        }
        Ok(RunResult {
            status: "step".into(),
            message: format!("Executed {}.", candidate.description),
            steps: 1,
            elapsed_ms: started.elapsed().as_millis(),
            reason: None,
            choice: Some(decision.choice),
            confidence: Some(decision.confidence),
            probabilities: decision.probabilities,
        })
    }

    pub fn run(&self, goal: &str) -> Result<RunResult, AgentError> {
        let started = Instant::now();
        let mut previous = CandidateContext::default();
        let mut steps = 0;
        let mut history = Vec::new();
        let mut carried_focus = None;
        while steps < self.config.max_steps {
            if started.elapsed() >= self.config.run_timeout {
                return Ok(self.blocked(steps, started, "run_timeout"));
            }
            let speculative = self.speculative_decision(goal, &previous, &history, steps)?;
            self.trace_speculative(steps, &speculative.timing);
            let mut observation = speculative.observation;
            if observation.focused_element_id.is_none() {
                observation.focused_element_id = carried_focus.clone();
            }
            let current_fingerprint = fingerprint(&observation);
            let mut candidates = speculative.candidates;
            let decision = speculative.decision;
            let action_result = speculative.action_result;
            self.trace_event(
                steps,
                "decision",
                started.elapsed().as_millis(),
                candidates.len(),
                Some(&decision.choice),
                decision.confidence,
                &decision.probabilities,
                None,
                find_candidate(&candidates, &decision.choice),
            );
            if decision.confidence < self.config.confidence_threshold {
                return Ok(self.blocked_for_decision(steps, started, "low_confidence", decision));
            }
            let candidate = find_candidate(&candidates, &decision.choice)
                .ok_or_else(|| AgentError::Config("Jev selected an unknown candidate".into()))?
                .clone();
            if candidate.terminal {
                return Ok(self.terminal_result(&candidate, steps, started));
            }
            if !candidate.terminal {
                candidates.retain(|item| item.id == candidate.id);
            }
            let clicked_editable = candidate.kind == ActionKind::Click
                && candidate
                    .target
                    .as_deref()
                    .and_then(|id| observation.elements.iter().find(|element| element.id == id))
                    .map(|element| element.is_editable())
                    .unwrap_or(false);
            let action_key = format!(
                "{}:{}",
                candidate.kind.label(),
                candidate
                    .target
                    .clone()
                    .or(candidate.literal.clone())
                    .unwrap_or_default()
            );
            let action_result = match action_result {
                Some(result) => result,
                None => self.desktopctl.execute(&candidate)?,
            };
            carried_focus = focused_element_id(&action_result);
            steps += 1;
            history.push(candidate.description.clone());
            if history.len() > 6 {
                history.remove(0);
            }
            let mut next = self.desktopctl.observe()?;
            if next.focused_element_id.is_none() {
                next.focused_element_id = carried_focus.clone();
            }
            let next_fingerprint = fingerprint(&next);
            let changed = next_fingerprint != current_fingerprint;
            if !changed {
                previous.repeated_actions.insert(action_key);
            } else {
                previous.repeated_actions.clear();
            }
            previous.previously_clicked_editable = clicked_editable && changed;
            previous.previous_kind = Some(candidate.kind.clone());
            previous.previous_fingerprint = Some(current_fingerprint);
            if !changed && previous.repeated_actions.len() >= 3 {
                return Ok(self.blocked(steps, started, "no_progress"));
            }
        }
        Ok(self.blocked(steps, started, "max_steps"))
    }

    fn choose(
        &self,
        goal: &str,
        observation: &Observation,
        candidates: &[Candidate],
    ) -> Result<crate::model::JevDecision, AgentError> {
        self.choose_with_history(goal, observation, candidates, &[])
    }

    fn choose_with_history(
        &self,
        goal: &str,
        observation: &Observation,
        candidates: &[Candidate],
        history: &[String],
    ) -> Result<crate::model::JevDecision, AgentError> {
        let criteria = candidates
            .iter()
            .map(|candidate| (candidate.id.clone(), candidate.criteria.clone()))
            .collect::<BTreeMap<_, _>>();
        self.jev
            .choose(
                build_state(goal, observation, &self.initial_context, history),
                criteria,
            )
            .map_err(AgentError::from)
    }

    fn speculative_decision(
        &self,
        goal: &str,
        context: &CandidateContext,
        history: &[String],
        _step: u32,
    ) -> Result<SpeculativeDecision, AgentError> {
        let menu_started = Instant::now();
        let menus = self.desktopctl.observe_menu_only()?;
        let menu_ms = menu_started.elapsed().as_millis();
        let mut menu_observation = Observation::default();
        menu_observation.active_window_id = self.config.active_window.clone();
        menu_observation.menus = menus.clone();

        let (exact_match, exact_candidate) = exact_menu_match(goal, &menu_observation);
        if exact_match == ExactMenuMatch::Unique {
            let candidate = exact_candidate.expect("unique exact menu match has a candidate");
            let action_result = self.desktopctl.execute(&candidate)?;
            return Ok(SpeculativeDecision {
                observation: menu_observation,
                candidates: vec![candidate.clone()],
                decision: crate::model::JevDecision {
                    choice: candidate.id.clone(),
                    confidence: 1.0,
                    probabilities: [(candidate.id.clone(), 1.0)].into_iter().collect(),
                    model: None,
                },
                action_result: Some(action_result),
                timing: SpeculativeTiming {
                    menu_ms,
                    tokenize_ms: None,
                    jev_ms: 0,
                    path: "deterministic_menu",
                },
            });
        }

        let menu_candidates = generate(goal, &menu_observation, context);
        let screen = self.desktopctl.start_screen_tokenize();
        let jev_started = Instant::now();
        let menu_decision =
            self.choose_with_history(goal, &menu_observation, &menu_candidates, history);
        let menu_jev_ms = jev_started.elapsed().as_millis();
        if let Ok(decision) = &menu_decision {
            if decision.confidence >= self.config.confidence_threshold {
                if let Some(candidate) = find_candidate(&menu_candidates, &decision.choice)
                    .filter(|candidate| candidate.kind == ActionKind::Menu && !candidate.terminal)
                {
                    let candidate = candidate.clone();
                    // The daemon serializes commands and killing this client
                    // would not cancel daemon-side work. Join the worker so
                    // menu execution is not queued behind tokenization.
                    let screen_started = screen.started;
                    let _ = screen.join();
                    let tokenize_ms = Some(screen_started.elapsed().as_millis());
                    let action_result = self.desktopctl.execute(&candidate)?;
                    return Ok(SpeculativeDecision {
                        observation: menu_observation,
                        candidates: menu_candidates,
                        decision: decision.clone(),
                        action_result: Some(action_result),
                        timing: SpeculativeTiming {
                            menu_ms,
                            tokenize_ms,
                            jev_ms: menu_jev_ms,
                            path: "menu_jev",
                        },
                    });
                }
            }
        }

        let screen_started = screen.started;
        let mut observation = screen.join()??;
        let tokenize_ms = screen_started.elapsed().as_millis();
        observation.menus = menus;
        let candidates = generate(goal, &observation, context);
        let jev_started = Instant::now();
        let decision = self.choose_with_history(goal, &observation, &candidates, history)?;
        let full_jev_ms = jev_started.elapsed().as_millis();
        Ok(SpeculativeDecision {
            observation,
            candidates,
            decision,
            action_result: None,
            timing: SpeculativeTiming {
                menu_ms,
                tokenize_ms: Some(tokenize_ms),
                jev_ms: menu_jev_ms + full_jev_ms,
                path: if menu_decision.is_err() {
                    "full_retry_after_menu_error"
                } else if exact_match == ExactMenuMatch::Ambiguous {
                    "full_after_ambiguous_exact"
                } else {
                    "full_after_menu_jev"
                },
            },
        })
    }

    fn terminal_result(&self, candidate: &Candidate, steps: u32, started: Instant) -> RunResult {
        let status = match candidate.status {
            Some(TerminalStatus::Done) => "done",
            _ => "blocked",
        };
        RunResult {
            status: status.into(),
            message: candidate.description.clone(),
            steps,
            elapsed_ms: started.elapsed().as_millis(),
            reason: (status == "blocked").then(|| "blocked".into()),
            choice: Some(candidate.id.clone()),
            confidence: None,
            probabilities: BTreeMap::new(),
        }
    }

    fn blocked(&self, steps: u32, started: Instant, reason: &str) -> RunResult {
        RunResult {
            status: "blocked".into(),
            message: format!("Desktop agent blocked: {reason}."),
            steps,
            elapsed_ms: started.elapsed().as_millis(),
            reason: Some(reason.into()),
            choice: None,
            confidence: None,
            probabilities: BTreeMap::new(),
        }
    }

    fn blocked_for_decision(
        &self,
        steps: u32,
        started: Instant,
        reason: &str,
        decision: crate::model::JevDecision,
    ) -> RunResult {
        let mut result = self.blocked(steps, started, reason);
        result.choice = Some(decision.choice);
        result.confidence = Some(decision.confidence);
        result.probabilities = decision.probabilities;
        result
    }

    fn trace_event(
        &self,
        step: u32,
        event: &str,
        elapsed_ms: u128,
        candidate_count: usize,
        choice: Option<&str>,
        confidence: f64,
        probabilities: &BTreeMap<String, f64>,
        state_change: Option<bool>,
        selected: Option<&Candidate>,
    ) {
        if !self.config.trace && self.config.trace_file.is_none() {
            return;
        }
        let event = serde_json::json!({
            "run_id": self.run_id,
            "step": step,
            "event": event,
            "elapsed_ms": elapsed_ms,
            "candidate_count": candidate_count,
            "choice": choice,
            "selected_action": selected.map(|candidate| serde_json::json!({
                "kind": candidate.kind,
                "target": candidate.target,
                "description": candidate.description,
            })),
            "confidence": confidence,
            "probabilities": probabilities,
            "state_changed": state_change,
        });
        let line = format!("{}\n", event);
        if self.config.trace {
            eprint!("{line}");
        }
        if let Some(path) = &self.config.trace_file {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
                let _ = file.write_all(line.as_bytes());
            }
        }
    }

    fn trace_speculative(&self, step: u32, timing: &SpeculativeTiming) {
        if !self.config.trace && self.config.trace_file.is_none() {
            return;
        }
        let event = serde_json::json!({
            "run_id": self.run_id,
            "step": step,
            "event": "speculative",
            "path": timing.path,
            "menu_ms": timing.menu_ms,
            "tokenize_ms": timing.tokenize_ms,
            "jev_ms": timing.jev_ms,
        });
        let line = format!("{}\n", event);
        if self.config.trace {
            eprint!("{line}");
        }
        if let Some(path) = &self.config.trace_file {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
                let _ = file.write_all(line.as_bytes());
            }
        }
    }
}

fn find_candidate<'a>(candidates: &'a [Candidate], id: &str) -> Option<&'a Candidate> {
    candidates.iter().find(|candidate| candidate.id == id)
}

fn fingerprint(observation: &Observation) -> String {
    let elements = observation
        .elements
        .iter()
        .map(|element| {
            format!(
                "{}:{}",
                element.id,
                element.text.as_deref().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("|");
    format!(
        "{}|{}|{}",
        observation.window.id.as_deref().unwrap_or_default(),
        observation
            .focused_element_id
            .as_deref()
            .unwrap_or_default(),
        elements
    )
}

fn build_state(
    goal: &str,
    observation: &Observation,
    initial_context: &Option<String>,
    history: &[String],
) -> String {
    let mut state = format!("USER GOAL\n{goal}\n\n");
    if let Some(context) = initial_context {
        state.push_str(&format!(
            "INITIAL CONTEXT\n{}\n\n",
            context.chars().take(4_000).collect::<String>()
        ));
    }
    state.push_str(&format!(
        "CURRENT WINDOW\nApplication: {}\nTitle: {}\n\nCURRENT FOCUS\n{}\n\nVISIBLE UI\n",
        observation.window.app.as_deref().unwrap_or("unknown"),
        observation.window.title.as_deref().unwrap_or("unknown"),
        observation.focused_element_id.as_deref().unwrap_or("none")
    ));
    for element in observation.elements.iter().take(120) {
        state.push_str(&format!(
            "- {} {:?} ({})\n",
            element.role,
            element.text.as_deref().unwrap_or_default(),
            element.id
        ));
    }
    state.push_str("\nAVAILABLE MENUS\n");
    for menu in observation.menus.iter().take(120) {
        state.push_str(&format!(
            "- {} [{}] ({})\n",
            menu.path,
            if menu.enabled { "enabled" } else { "disabled" },
            menu.id
        ));
    }
    state.push_str("\nRECENT ACTIONS\n");
    for (index, action) in history.iter().enumerate() {
        state.push_str(&format!("{}. {}\n", index + 1, action));
    }
    state
}
