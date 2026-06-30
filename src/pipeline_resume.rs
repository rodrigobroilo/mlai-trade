// Resumable checkpoints for long data/ML prep pipelines.
//
// Function map:
// - PipelineResume::open(): loads or initializes the active checkpoint.
// - PipelineResume::begin_step()/complete_step()/fail_step(): records step state.
// - PipelineResume::finish(): removes the checkpoint after full success.

use crate::{logging, paths};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PipelineResumeProfile {
    pub name: String,
    pub parameters: BTreeMap<String, String>,
    pub binary_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStep {
    pub id: String,
    pub index: usize,
    pub total: usize,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompletedStep {
    id: String,
    index: usize,
    total: usize,
    label: String,
    completed_at_utc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FailedStep {
    id: String,
    index: usize,
    total: usize,
    label: String,
    failed_at_utc: String,
    error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PipelineResumeState {
    version: u32,
    profile: PipelineResumeProfile,
    started_at_utc: String,
    updated_at_utc: String,
    completed_steps: BTreeMap<String, CompletedStep>,
    last_failed_step: Option<FailedStep>,
}

pub struct PipelineResume {
    path: PathBuf,
    state: PipelineResumeState,
}

impl PipelineResumeProfile {
    pub fn new(name: impl Into<String>, parameters: BTreeMap<String, String>) -> Self {
        Self {
            name: name.into(),
            parameters,
            binary_fingerprint: current_binary_fingerprint(),
        }
    }
}

impl PipelineStep {
    pub fn new(id: &'static str, index: usize, total: usize, label: &'static str) -> Self {
        Self {
            id: id.to_string(),
            index,
            total,
            label: label.to_string(),
        }
    }
}

impl PipelineResume {
    pub fn open(profile: PipelineResumeProfile, restart: bool) -> anyhow::Result<Self> {
        let path = checkpoint_path();
        if restart && path.exists() {
            let _ = fs::remove_file(&path);
            log_event(serde_json::json!({
                "event": "pipeline_resume_checkpoint_reset",
                "level": "warn",
                "reason": "--restart",
                "checkpoint_file": path.display().to_string(),
            }));
        }

        if let Some(mut state) = read_state(&path)? {
            if state.version == STATE_VERSION && state.profile == profile {
                state.updated_at_utc = utc_now();
                let completed = state.completed_steps.len();
                if completed > 0 {
                    println!(
                        "  Resume: {} completed step(s) loaded from {}",
                        completed,
                        path.display()
                    );
                } else {
                    println!("  Resume: checkpoint active at {}", path.display());
                }
                write_state(&path, &state)?;
                log_event(serde_json::json!({
                    "event": "pipeline_resume_checkpoint_loaded",
                    "level": "info",
                    "profile": state.profile.name,
                    "completed_steps": completed,
                    "checkpoint_file": path.display().to_string(),
                }));
                return Ok(Self { path, state });
            }

            log_event(serde_json::json!({
                "event": "pipeline_resume_checkpoint_reset",
                "level": "warn",
                "reason": "profile_changed",
                "old_profile": state.profile.name,
                "new_profile": profile.name,
                "checkpoint_file": path.display().to_string(),
            }));
            let _ = fs::remove_file(&path);
        }

        let now = utc_now();
        let state = PipelineResumeState {
            version: STATE_VERSION,
            profile,
            started_at_utc: now.clone(),
            updated_at_utc: now,
            completed_steps: BTreeMap::new(),
            last_failed_step: None,
        };
        write_state(&path, &state)?;
        println!("  Resume: checkpoint file {}", path.display());
        log_event(serde_json::json!({
            "event": "pipeline_resume_checkpoint_started",
            "level": "info",
            "profile": state.profile.name,
            "checkpoint_file": path.display().to_string(),
        }));
        Ok(Self { path, state })
    }

    pub fn begin_step(&self, step: &PipelineStep) -> bool {
        if let Some(completed) = self.state.completed_steps.get(&step.id) {
            println!(
                "\n{}/{} {} (resume: already completed at {})",
                step.index, step.total, step.label, completed.completed_at_utc
            );
            log_event(serde_json::json!({
                "event": "pipeline_resume_step_skipped",
                "level": "info",
                "profile": self.state.profile.name,
                "step_id": step.id,
                "step_index": step.index,
                "step_total": step.total,
                "step_label": step.label,
                "completed_at_utc": completed.completed_at_utc,
                "checkpoint_file": self.path.display().to_string(),
            }));
            return false;
        }

        println!("\n{}/{} {}", step.index, step.total, step.label);
        log_event(serde_json::json!({
            "event": "pipeline_resume_step_started",
            "level": "info",
            "profile": self.state.profile.name,
            "step_id": step.id,
            "step_index": step.index,
            "step_total": step.total,
            "step_label": step.label,
            "checkpoint_file": self.path.display().to_string(),
        }));
        true
    }

    pub fn complete_step(&mut self, step: &PipelineStep) -> anyhow::Result<()> {
        let now = utc_now();
        self.state.completed_steps.insert(
            step.id.clone(),
            CompletedStep {
                id: step.id.clone(),
                index: step.index,
                total: step.total,
                label: step.label.clone(),
                completed_at_utc: now.clone(),
            },
        );
        self.state.last_failed_step = None;
        self.state.updated_at_utc = now.clone();
        write_state(&self.path, &self.state)?;
        log_event(serde_json::json!({
            "event": "pipeline_resume_step_completed",
            "level": "info",
            "profile": self.state.profile.name,
            "step_id": step.id,
            "step_index": step.index,
            "step_total": step.total,
            "step_label": step.label,
            "completed_at_utc": now,
            "completed_steps": self.state.completed_steps.len(),
            "checkpoint_file": self.path.display().to_string(),
        }));
        Ok(())
    }

    pub fn fail_step(&mut self, step: &PipelineStep, err: &anyhow::Error) -> anyhow::Result<()> {
        let now = utc_now();
        self.state.last_failed_step = Some(FailedStep {
            id: step.id.clone(),
            index: step.index,
            total: step.total,
            label: step.label.clone(),
            failed_at_utc: now.clone(),
            error: err.to_string(),
        });
        self.state.updated_at_utc = now.clone();
        write_state(&self.path, &self.state)?;
        log_event(serde_json::json!({
            "event": "pipeline_resume_step_failed",
            "level": "error",
            "profile": self.state.profile.name,
            "step_id": step.id,
            "step_index": step.index,
            "step_total": step.total,
            "step_label": step.label,
            "failed_at_utc": now,
            "error": err.to_string(),
            "completed_steps": self.state.completed_steps.len(),
            "checkpoint_file": self.path.display().to_string(),
        }));
        Ok(())
    }

    pub fn finish(self) {
        let completed = self.state.completed_steps.len();
        if self.path.exists() {
            let _ = fs::remove_file(&self.path);
        }
        log_event(serde_json::json!({
            "event": "pipeline_resume_checkpoint_finished",
            "level": "info",
            "profile": self.state.profile.name,
            "completed_steps": completed,
            "checkpoint_file": self.path.display().to_string(),
        }));
    }
}

fn checkpoint_path() -> PathBuf {
    paths::tmp_dir().join("mlai-trade-pipeline-resume.json")
}

fn utc_now() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn current_binary_fingerprint() -> String {
    let current = std::env::current_exe()
        .ok()
        .and_then(|path| {
            let metadata = fs::metadata(&path).ok()?;
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            Some(format!(
                "{}:{}:{}",
                path.display(),
                metadata.len(),
                modified
            ))
        })
        .unwrap_or_else(|| "unknown".to_string());
    current
}

fn read_state(path: &PathBuf) -> anyhow::Result<Option<PipelineResumeState>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    match serde_json::from_str::<PipelineResumeState>(&text) {
        Ok(state) => Ok(Some(state)),
        Err(err) => {
            log_event(serde_json::json!({
                "event": "pipeline_resume_checkpoint_reset",
                "level": "warn",
                "reason": "invalid_json",
                "error": err.to_string(),
                "checkpoint_file": path.display().to_string(),
            }));
            let _ = fs::remove_file(path);
            Ok(None)
        }
    }
}

fn write_state(path: &PathBuf, state: &PipelineResumeState) -> anyhow::Result<()> {
    let content = serde_json::to_string_pretty(state)?;
    let tmp_path = path.with_file_name(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("mlai-trade-pipeline-resume.json")
    ));
    paths::write_private_file(&tmp_path, content)?;
    fs::rename(&tmp_path, path)?;
    let _ = paths::harden_file_if_exists(path);
    Ok(())
}

fn log_event(event: serde_json::Value) {
    for component in ["daemon", "data", "ml", "training"] {
        logging::append_component_event_lossy(component, event.clone());
    }
}
