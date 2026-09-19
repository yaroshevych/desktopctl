use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::candidates::{generate, CandidateContext};
use crate::desktopctl::{focused_element_id, DesktopCtl, DesktopCtlError};
use crate::jev::{JevClient, JevError};
use crate::model::{ActionKind, Candidate, Observation, TerminalStatus};

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
        self.trace_event(
            0,
            "inspect",
            started.elapsed().as_millis(),
            candidates.len(),
            Some(&decision.choice),
            decision.confidence,
            &decision.probabilities,
            None,
        );
        Ok(RunResult {
            status: "inspect".into(),
            message: format!(
                "Jev chose {} (confidence {:.2}).",
                decision.choice, decision.confidence
            ),
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
        let observation = self.desktopctl.observe()?;
        let context = CandidateContext::default();
        let candidates = generate(goal, &observation, &context);
        let decision = self.choose(goal, &observation, &candidates)?;
        self.trace_event(
            0,
            "decision",
            started.elapsed().as_millis(),
            candidates.len(),
            Some(&decision.choice),
            decision.confidence,
            &decision.probabilities,
            None,
        );
        if decision.confidence < self.config.confidence_threshold {
            return Ok(self.blocked(1, started, "low_confidence"));
        }
        let candidate = find_candidate(&candidates, &decision.choice)
            .ok_or_else(|| AgentError::Config("Jev selected an unknown candidate".into()))?;
        if candidate.terminal {
            return Ok(self.terminal_result(candidate, 0, started));
        }
        self.desktopctl.execute(candidate)?;
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
            let mut observation = self.desktopctl.observe()?;
            if observation.focused_element_id.is_none() {
                observation.focused_element_id = carried_focus.clone();
            }
            let current_fingerprint = fingerprint(&observation);
            let mut candidates = generate(goal, &observation, &previous);
            let decision = self.choose_with_history(goal, &observation, &candidates, &history)?;
            self.trace_event(
                steps,
                "decision",
                started.elapsed().as_millis(),
                candidates.len(),
                Some(&decision.choice),
                decision.confidence,
                &decision.probabilities,
                None,
            );
            if decision.confidence < self.config.confidence_threshold {
                return Ok(self.blocked(steps, started, "low_confidence"));
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
            let action_result = self.desktopctl.execute(&candidate)?;
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
            reason: Some(status.into()),
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
    ) {
        if !self.config.trace && self.config.trace_file.is_none() {
            return;
        }
        let event = serde_json::json!({"run_id":self.run_id,"step":step,"event":event,"elapsed_ms":elapsed_ms,"candidate_count":candidate_count,"choice":choice,"confidence":confidence,"probabilities":probabilities,"state_changed":state_change});
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
    state.push_str("\nRECENT ACTIONS\n");
    for (index, action) in history.iter().enumerate() {
        state.push_str(&format!("{}. {}\n", index + 1, action));
    }
    state
}
