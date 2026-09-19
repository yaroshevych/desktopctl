use std::collections::BTreeMap;
use std::time::Duration;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::model::JevDecision;

#[derive(Debug, Error)]
pub enum JevError {
    #[error("TYPESAFE_API_KEY is not set")]
    MissingKey,
    #[error("Jev request failed: {0}")]
    Request(String),
    #[error("Jev returned HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("invalid Jev response: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Serialize)]
struct ChoiceQuestion {
    #[serde(rename = "type")]
    kind: &'static str,
    instructions: String,
    criteria: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
struct RequestBody {
    state: String,
    model: String,
    questions: BTreeMap<String, ChoiceQuestion>,
}

#[derive(Debug, Deserialize)]
struct ResponseBody {
    #[allow(dead_code)]
    model: Option<String>,
    answers: BTreeMap<String, Answer>,
}

#[derive(Debug, Deserialize)]
struct Answer {
    #[serde(rename = "type")]
    kind: String,
    choice: Option<String>,
    probabilities: Option<BTreeMap<String, f64>>,
    confidence: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct JevClient {
    client: Client,
    api_key: String,
    model: String,
    endpoint: String,
}

impl JevClient {
    pub fn new(timeout: Duration) -> Result<Self, JevError> {
        let api_key = std::env::var("TYPESAFE_API_KEY").map_err(|_| JevError::MissingKey)?;
        if api_key.trim().is_empty() {
            return Err(JevError::MissingKey);
        }
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| JevError::Request(e.to_string()))?;
        Ok(Self {
            client,
            api_key,
            model: std::env::var("DESKTOP_AGENT_JEV_MODEL").unwrap_or_else(|_| "jev-latest".into()),
            endpoint: std::env::var("DESKTOP_AGENT_JEV_URL")
                .unwrap_or_else(|_| "https://api.typesafe.ai/v1/systemone".into()),
        })
    }

    pub fn choose(
        &self,
        state: String,
        criteria: BTreeMap<String, String>,
    ) -> Result<JevDecision, JevError> {
        let body = RequestBody {
            state,
            model: self.model.clone(),
            questions: [("next_action".into(), ChoiceQuestion {
                kind: "choice",
                instructions: "Choose the single next desktop action that best advances the user's goal given the current UI state. Choose done only when the goal is visibly complete. Choose blocked only when no available action can advance it.".into(),
                criteria,
            })].into_iter().collect(),
        };
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .map_err(|e| JevError::Request(e.to_string()))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .map_err(|e| JevError::Request(e.to_string()))?;
        if !status.is_success() {
            let message = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| "request rejected".into());
            return Err(JevError::Http {
                status: status.as_u16(),
                message,
            });
        }
        let parsed: ResponseBody = serde_json::from_slice(&bytes)?;
        let answer = parsed
            .answers
            .get("next_action")
            .ok_or_else(|| JevError::Request("response omitted next_action".into()))?;
        if answer.kind != "choice" {
            return Err(JevError::Request(
                "next_action was not a Choice answer".into(),
            ));
        }
        let choice = answer
            .choice
            .clone()
            .ok_or_else(|| JevError::Request("Choice answer omitted choice".into()))?;
        let probabilities = answer.probabilities.clone().unwrap_or_default();
        let confidence = answer
            .confidence
            .unwrap_or_else(|| probabilities.get(&choice).copied().unwrap_or(0.0));
        Ok(JevDecision {
            choice,
            confidence,
            probabilities,
            model: parsed.model,
        })
    }
}
